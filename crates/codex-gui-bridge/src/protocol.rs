//! Shared protocol helpers for the codex-gui-bridge broker.

use serde_json::{json, Value};

/// Prefix for request ids injected by the bridge. Desktop's own request ids are
/// plain numbers (or whatever the Desktop client chooses); using a string prefix
/// guarantees we never collide with them.
pub const INJECTED_ID_PREFIX: &str = "__bridge_";

/// True when a JSON-RPC message carries an id that belongs to an injected
/// request (i.e. one originated by the CLI through the broker).
pub fn is_injected_response(message: &Value) -> bool {
    message
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| id.starts_with(INJECTED_ID_PREFIX))
}

/// True when the message has an id at all (a request or a response, as opposed
/// to a notification which carries no id).
pub fn has_id(message: &Value) -> bool {
    message.get("id").is_some()
}

/// Stable map key for either a numeric or string JSON-RPC request id.
pub fn request_id_key(message: &Value) -> Option<String> {
    let id = message.get("id")?;
    match id {
        Value::String(id) => Some(format!("s:{id}")),
        Value::Number(id) => Some(format!("n:{id}")),
        _ => None,
    }
}

/// True when the message is a notification (no id field).
pub fn is_notification(message: &Value) -> bool {
    !has_id(message)
}

/// Extract the `method` of a request message, if present.
pub fn method_of(message: &Value) -> Option<&str> {
    message.get("method").and_then(Value::as_str)
}

/// Extract the `threadId` from request params, if present.
pub fn thread_id_of(message: &Value) -> Option<String> {
    message
        .get("params")
        .and_then(|params| params.get("threadId"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Extract a thread id from either a client request or the app-server shapes
/// that identify the thread created/resumed by the GUI. Current v2 schemas use
/// `result.thread.id` for thread/start and thread/resume responses, and
/// `params.thread.id` for the thread/started notification.
pub fn observed_thread_id(message: &Value) -> Option<String> {
    thread_id_of(message).or_else(|| {
        [
            &["result", "thread", "id"][..],
            &["params", "thread", "id"][..],
        ]
        .into_iter()
        .find_map(|path| {
            value_at_path(message, path)
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    })
}

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(value, |current, key| current.get(key))
}

/// Build a JSON-RPC style request with an injected (string) id.
///
/// The app-server wire format omits `"jsonrpc":"2.0"`, so we do too.
pub fn injected_request(id: u64, method: &str, params: Value) -> Value {
    json!({
        "id": format!("{INJECTED_ID_PREFIX}{id:06}"),
        "method": method,
        "params": params,
    })
}

/// Build the `initialize` request the bridge sends on its own upstream
/// connection (used for read-only probes before any Desktop client attaches).
pub fn initialize_request(client_name: &str, version: &str) -> Value {
    json!({
        "id": 0,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": client_name,
                "title": client_name,
                "version": version,
            },
            "capabilities": { "experimentalApi": true },
        },
    })
}

/// The `initialized` notification sent after a successful initialize.
pub fn initialized_notification() -> Value {
    json!({ "method": "initialized" })
}

/// Build a `turn/start` request for sending a text message into a thread.
pub fn turn_start_request(id: u64, thread_id: &str, text: &str) -> Value {
    injected_request(
        id,
        "turn/start",
        json!({
            "threadId": thread_id,
            "input": [{
                "type": "text",
                "text": text,
                "text_elements": [],
            }],
        }),
    )
}

/// Build a `turn/steer` request for injecting input into an active turn.
pub fn turn_steer_request(id: u64, thread_id: &str, turn_id: &str, text: &str) -> Value {
    injected_request(
        id,
        "turn/steer",
        json!({
            "threadId": thread_id,
            "input": [{
                "type": "text",
                "text": text,
                "text_elements": [],
            }],
            "expectedTurnId": turn_id,
        }),
    )
}

/// Build a `turn/interrupt` request.
pub fn turn_interrupt_request(id: u64, thread_id: &str, turn_id: &str) -> Value {
    injected_request(
        id,
        "turn/interrupt",
        json!({
            "threadId": thread_id,
            "turnId": turn_id,
        }),
    )
}

/// Build a `thread/read` request.
pub fn thread_read_request(id: u64, thread_id: &str) -> Value {
    injected_request(id, "thread/read", json!({ "threadId": thread_id }))
}

/// Build a `thread/turns/list` request.
pub fn thread_turns_list_request(id: u64, thread_id: &str, limit: u64) -> Value {
    injected_request(
        id,
        "thread/turns/list",
        json!({
            "threadId": thread_id,
            "limit": limit,
        }),
    )
}

/// Build a `thread/list` request.
pub fn thread_list_request(id: u64, limit: u64) -> Value {
    injected_request(id, "thread/list", json!({ "limit": limit }))
}

/// Extract a human-readable error string from an app-server error object.
pub fn error_message(message: &Value) -> String {
    if let Some(error) = message.get("error") {
        let code = error.get("code").map(Value::to_string).unwrap_or_default();
        let msg = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown app-server error");
        return format!("app-server error {code}: {msg}");
    }
    "app-server response has neither result nor error".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_id_detection() {
        assert!(is_injected_response(
            &json!({"id": "__bridge_000042", "result": {}})
        ));
        assert!(!is_injected_response(&json!({"id": 42, "result": {}})));
        assert!(!is_injected_response(&json!({"id": "42", "result": {}})));
        assert!(!is_injected_response(
            &json!({"method": "turn/started", "params": {}})
        ));
    }

    #[test]
    fn notification_detection() {
        assert!(is_notification(
            &json!({"method": "turn/started", "params": {}})
        ));
        assert!(!is_notification(
            &json!({"id": 1, "method": "turn/start", "params": {}})
        ));
        assert_eq!(
            request_id_key(&json!({"id": 1, "method": "turn/start"})).as_deref(),
            Some("n:1")
        );
        assert_eq!(
            request_id_key(&json!({"id": "1", "method": "turn/start"})).as_deref(),
            Some("s:1")
        );
    }

    #[test]
    fn thread_id_extraction() {
        let request = json!({
            "id": 1,
            "method": "thread/start",
            "params": {"threadId": "abc-123", "cwd": "/tmp"}
        });
        assert_eq!(thread_id_of(&request).as_deref(), Some("abc-123"));
        assert_eq!(thread_id_of(&json!({"method": "x"})), None);

        assert_eq!(
            observed_thread_id(&json!({
                "id": 7,
                "result": {"thread": {"id": "created-thread"}}
            }))
            .as_deref(),
            Some("created-thread")
        );
        assert_eq!(
            observed_thread_id(&json!({
                "method": "thread/started",
                "params": {"thread": {"id": "notified-thread"}}
            }))
            .as_deref(),
            Some("notified-thread")
        );
    }

    #[test]
    fn injected_request_shape() {
        let request = turn_start_request(42, "thr-1", "hello");
        assert_eq!(request["id"], "__bridge_000042");
        assert_eq!(request["method"], "turn/start");
        assert_eq!(request["params"]["threadId"], "thr-1");
        assert_eq!(request["params"]["input"][0]["text"], "hello");
        assert!(request.get("jsonrpc").is_none());
    }
}
