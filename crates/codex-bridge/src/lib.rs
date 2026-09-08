use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

mod app_server_schema;
mod host_executor;
mod sessions;
mod write_backend;

pub use app_server_schema::APP_SERVER_SCHEMA_VERSION;

pub use host_executor::{
    HostExecFailure, HostExecPolicyConfig, HostExecPolicySummary, HostExecResult, HostExecutor,
    DEFAULT_HOST_EXEC_TIMEOUT_SECONDS, HOST_EXEC_OUTPUT_LIMIT_BYTES, HOST_EXEC_POLICY_ENV,
    MAX_HOST_EXEC_TIMEOUT_SECONDS,
};
pub use sessions::{
    default_codex_home, MessagePage, ProjectSummary, ProjectThreadSummary, SessionStore,
    ThreadActivity, ThreadMessage, ThreadSnapshot, ThreadSummary, ThreadToolCall, CODEX_HOME_ENV,
};
pub use write_backend::{
    AppServerRuntimeInfo, BackendFailure, BackendSuccess, CodexCliBackend, NativeQueueReceipt,
    APP_SERVER_SOCKET_ENV, CODEX_BIN_ENV,
};

pub const PROTOCOL_VERSION: u32 = 20;
pub const SOCKET_ENV: &str = "CODEX_BRIDGE_SOCKET";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum Request {
    Ls {
        limit: u32,
        include_archived: bool,
    },
    Projects {
        include_archived: bool,
    },
    ProjectThreads {
        project_path: PathBuf,
        include_archived: bool,
        offset: u32,
        limit: u32,
    },
    Messages {
        thread_id: String,
        before: Option<u32>,
        limit: u32,
    },
    MessageContent {
        thread_id: String,
        message_index: u32,
        content_index: u32,
        content_end: Option<u32>,
    },
    ToolContent {
        thread_id: String,
        message_index: u32,
        tool_index: u32,
    },
    ThreadActivity {
        thread_id: String,
    },
    ThreadWatch {
        thread_id: String,
    },
    ComposerStatus {
        thread_id: String,
    },
    ComposerOptions,
    ThreadCreate {
        project_path: PathBuf,
        worktree: bool,
        model: Option<String>,
    },
    ThreadSettingsUpdate {
        thread_id: String,
        model: String,
        effort: String,
    },
    ThreadRename {
        thread_id: String,
        name: String,
    },
    ThreadArchive {
        thread_id: String,
    },
    ThreadPins,
    ThreadPin {
        thread_id: String,
        pinned: bool,
    },
    WorkspaceDiff {
        thread_id: String,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ComposerAttachment>,
    },
    Steer {
        thread_id: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ComposerAttachment>,
    },
    AudioTranscribe {
        thread_id: String,
        audio: RealtimeAudioChunk,
    },
    Scroll {
        direction: Option<ScrollDirection>,
        pixels: Option<i64>,
        target: Option<String>,
        message_id: Option<String>,
    },
    Pending,
    PendingMessages {
        thread_id: Option<String>,
    },
    PendingMessageDelete {
        id: String,
        thread_id: String,
    },
    Approve {
        id: u64,
    },
    Decline {
        id: u64,
    },
    Interrupt {
        thread_id: Option<String>,
    },
    HostExec {
        thread_id: Option<String>,
        argv: Vec<String>,
        timeout_seconds: Option<u64>,
    },
    AppServerRpc {
        method: String,
        params: serde_json::Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ComposerAttachment {
    Image {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Audio {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RealtimeAudioChunk {
    pub data: String,
    pub sample_rate: u32,
    pub num_channels: u16,
    pub samples_per_channel: u32,
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ls { .. } => "ls",
            Self::Projects { .. } => "projects",
            Self::ProjectThreads { .. } => "project_threads",
            Self::Messages { .. } => "messages",
            Self::MessageContent { .. } => "message_content",
            Self::ToolContent { .. } => "tool_content",
            Self::ThreadActivity { .. } => "thread_activity",
            Self::ThreadWatch { .. } => "thread_watch",
            Self::ComposerStatus { .. } => "composer_status",
            Self::ComposerOptions => "composer_options",
            Self::ThreadCreate { .. } => "thread_create",
            Self::ThreadSettingsUpdate { .. } => "thread_settings_update",
            Self::ThreadRename { .. } => "thread_rename",
            Self::ThreadArchive { .. } => "thread_archive",
            Self::ThreadPins => "thread_pins",
            Self::ThreadPin { .. } => "thread_pin",
            Self::WorkspaceDiff { .. } => "workspace_diff",
            Self::Select { .. } => "select",
            Self::Current => "current",
            Self::Status => "status",
            Self::Show { .. } => "show",
            Self::Tail => "tail",
            Self::Send { .. } => "send",
            Self::Steer { .. } => "steer",
            Self::AudioTranscribe { .. } => "audio_transcribe",
            Self::Scroll { .. } => "scroll",
            Self::Pending => "pending",
            Self::PendingMessages { .. } => "pending_messages",
            Self::PendingMessageDelete { .. } => "pending_message_delete",
            Self::Approve { .. } => "approve",
            Self::Decline { .. } => "decline",
            Self::Interrupt { .. } => "interrupt",
            Self::HostExec { .. } => "host_exec",
            Self::AppServerRpc { .. } => "app_server_rpc",
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
    fn web_index_and_message_requests_are_separately_pageable() {
        let projects = serde_json::to_value(Request::Projects {
            include_archived: false,
        })
        .unwrap();
        assert_eq!(projects["command"], "projects");

        let threads = serde_json::to_value(Request::ProjectThreads {
            project_path: PathBuf::from("/tmp/project"),
            include_archived: false,
            offset: 50,
            limit: 50,
        })
        .unwrap();
        assert_eq!(threads["command"], "project_threads");
        assert_eq!(threads["offset"], 50);

        let messages = serde_json::to_value(Request::Messages {
            thread_id: "thread-1".into(),
            before: Some(90),
            limit: 30,
        })
        .unwrap();
        assert_eq!(messages["command"], "messages");
        assert_eq!(messages["before"], 90);
        assert_eq!(messages["limit"], 30);

        let content = serde_json::to_value(Request::MessageContent {
            thread_id: "thread-1".into(),
            message_index: 12,
            content_index: 2,
            content_end: Some(8),
        })
        .unwrap();
        assert_eq!(content["command"], "message_content");
        assert_eq!(content["message_index"], 12);
        assert_eq!(content["content_end"], 8);

        let tool = serde_json::to_value(Request::ToolContent {
            thread_id: "thread-1".into(),
            message_index: 12,
            tool_index: 3,
        })
        .unwrap();
        assert_eq!(tool["command"], "tool_content");
        assert_eq!(tool["tool_index"], 3);

        let activity = serde_json::to_value(Request::ThreadActivity {
            thread_id: "thread-1".into(),
        })
        .unwrap();
        assert_eq!(activity["command"], "thread_activity");
        assert_eq!(activity["thread_id"], "thread-1");

        let composer = serde_json::to_value(Request::ComposerStatus {
            thread_id: "thread-1".into(),
        })
        .unwrap();
        assert_eq!(composer["command"], "composer_status");
        assert_eq!(composer["thread_id"], "thread-1");

        let create = serde_json::to_value(Request::ThreadCreate {
            project_path: PathBuf::from("/tmp/project"),
            worktree: true,
            model: Some("gpt-test".into()),
        })
        .unwrap();
        assert_eq!(create["command"], "thread_create");
        assert_eq!(create["project_path"], "/tmp/project");
        assert_eq!(create["worktree"], true);
        assert_eq!(create["model"], "gpt-test");

        let settings = serde_json::to_value(Request::ThreadSettingsUpdate {
            thread_id: "thread-1".into(),
            model: "gpt-test".into(),
            effort: "high".into(),
        })
        .unwrap();
        assert_eq!(settings["command"], "thread_settings_update");
        assert_eq!(settings["model"], "gpt-test");
        assert_eq!(settings["effort"], "high");

        let archive = serde_json::to_value(Request::ThreadArchive {
            thread_id: "thread-1".into(),
        })
        .unwrap();
        assert_eq!(archive["command"], "thread_archive");

        let rename = serde_json::to_value(Request::ThreadRename {
            thread_id: "thread-1".into(),
            name: "New name".into(),
        })
        .unwrap();
        assert_eq!(rename["command"], "thread_rename");
        assert_eq!(rename["name"], "New name");

        let pins = serde_json::to_value(Request::ThreadPins).unwrap();
        assert_eq!(pins["command"], "thread_pins");

        let pin = serde_json::to_value(Request::ThreadPin {
            thread_id: "thread-1".into(),
            pinned: true,
        })
        .unwrap();
        assert_eq!(pin["command"], "thread_pin");
        assert_eq!(pin["thread_id"], "thread-1");
        assert_eq!(pin["pinned"], true);

        let diff = serde_json::to_value(Request::WorkspaceDiff {
            thread_id: "thread-1".into(),
        })
        .unwrap();
        assert_eq!(diff["command"], "workspace_diff");
    }

    #[test]
    fn write_requests_carry_optional_thread_targets() {
        let request = Request::Send {
            thread_id: Some("thread-1".into()),
            text: "continue".into(),
            attachments: Vec::new(),
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "send");
        assert_eq!(json["thread_id"], "thread-1");
        assert_eq!(json["text"], "continue");
    }

    #[test]
    fn audio_transcribe_uses_pcm_metadata() {
        let request = Request::AudioTranscribe {
            thread_id: "thread-1".into(),
            audio: RealtimeAudioChunk {
                data: "AAAAAA==".into(),
                sample_rate: 24_000,
                num_channels: 1,
                samples_per_channel: 2,
            },
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "audio_transcribe");
        assert_eq!(json["audio"]["sample_rate"], 24_000);
        assert_eq!(json["audio"]["num_channels"], 1);
    }

    #[test]
    fn host_exec_request_uses_an_argv_array_without_a_shell_string() {
        let request = Request::HostExec {
            thread_id: Some("thread-1".into()),
            argv: vec!["wlink".into(), "flash".into(), "firmware.bin".into()],
            timeout_seconds: Some(120),
        };

        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "host_exec");
        assert_eq!(json["argv"][0], "wlink");
        assert_eq!(json["timeout_seconds"], 120);
    }

    #[test]
    fn native_app_server_request_carries_method_and_params() {
        let request = Request::AppServerRpc {
            method: "thread/read".into(),
            params: serde_json::json!({"threadId": "thread-1"}),
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "app_server_rpc");
        assert_eq!(json["method"], "thread/read");
        assert_eq!(json["params"]["threadId"], "thread-1");
    }

    #[test]
    fn pending_message_delete_targets_one_thread_entry() {
        let request = Request::PendingMessageDelete {
            id: "bridge-1".into(),
            thread_id: "thread-1".into(),
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["command"], "pending_message_delete");
        assert_eq!(json["id"], "bridge-1");
        assert_eq!(json["thread_id"], "thread-1");
    }
}
