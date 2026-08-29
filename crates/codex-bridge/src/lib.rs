use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const SOCKET_ENV: &str = "CODEX_BRIDGE_SOCKET";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Ls,
    Current,
    Status,
    Show {
        json: bool,
    },
    Tail,
    Send {
        text: String,
    },
    Steer {
        text: String,
    },
    Scroll {
        direction: Option<ScrollDirection>,
        pixels: Option<i64>,
        target: Option<String>,
        message_id: Option<String>,
    },
    Pending,
    Approve {
        id: u64,
    },
    Decline {
        id: u64,
    },
    Interrupt,
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ls => "ls",
            Self::Current => "current",
            Self::Status => "status",
            Self::Show { .. } => "show",
            Self::Tail => "tail",
            Self::Send { .. } => "send",
            Self::Steer { .. } => "steer",
            Self::Scroll { .. } => "scroll",
            Self::Pending => "pending",
            Self::Approve { .. } => "approve",
            Self::Decline { .. } => "decline",
            Self::Interrupt => "interrupt",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn success(result: serde_json::Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(ErrorBody {
                code: code.into(),
                message: message.into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: String,
    pub message: String,
}

pub fn default_socket_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os(SOCKET_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let home = env::var_os("HOME").context("HOME is not set; pass --socket explicitly")?;
    Ok(PathBuf::from(home)
        .join(".codex-bridge")
        .join("control.sock"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_uses_tagged_json_protocol() {
        let request = Request::Scroll {
            direction: Some(ScrollDirection::Down),
            pixels: None,
            target: None,
            message_id: None,
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "scroll");
        assert_eq!(json["direction"], "down");
    }

    #[test]
    fn error_response_omits_result() {
        let response = Response::error("not_implemented", "later");
        let json = serde_json::to_value(response).unwrap();

        assert_eq!(json["ok"], false);
        assert!(json.get("result").is_none());
        assert_eq!(json["error"]["code"], "not_implemented");
    }
}
