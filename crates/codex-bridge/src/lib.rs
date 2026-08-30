use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

mod sessions;
mod write_backend;

pub use sessions::{
    default_codex_home, SessionStore, ThreadMessage, ThreadSnapshot, ThreadSummary, CODEX_HOME_ENV,
};
pub use write_backend::{
    BackendFailure, BackendSuccess, CodexCliBackend, APP_SERVER_SOCKET_ENV, CODEX_BIN_ENV,
};

pub const PROTOCOL_VERSION: u32 = 3;
pub const SOCKET_ENV: &str = "CODEX_BRIDGE_SOCKET";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Ls {
        limit: u32,
        include_archived: bool,
    },
    Select {
        thread_id: String,
    },
    Current,
    Status,
    Show {
        thread_id: Option<String>,
        last: Option<u32>,
    },
    Tail,
    Send {
        thread_id: Option<String>,
        text: String,
    },
    Steer {
        thread_id: Option<String>,
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
    Interrupt {
        thread_id: Option<String>,
    },
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ls { .. } => "ls",
            Self::Select { .. } => "select",
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
            Self::Interrupt { .. } => "interrupt",
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

    #[test]
    fn list_request_carries_a_bounded_query() {
        let request = Request::Ls {
            limit: 25,
            include_archived: true,
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "ls");
        assert_eq!(json["limit"], 25);
        assert_eq!(json["include_archived"], true);
    }

    #[test]
    fn write_requests_carry_optional_thread_targets() {
        let request = Request::Send {
            thread_id: Some("thread-1".into()),
            text: "continue".into(),
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "send");
        assert_eq!(json["thread_id"], "thread-1");
        assert_eq!(json["text"], "continue");
    }
}
