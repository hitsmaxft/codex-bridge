use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::HashMap;

const PRIMARY_THREAD: &str = "demo-thread-web-ui";
const SECONDARY_THREAD: &str = "demo-thread-protocol";
const PROJECT_PATH: &str = "/Users/demo/projects/codex-bridge";
const CHATS_PROJECT_PATH: &str = "codex-bridge://chats";

thread_local! {
    static STATE: RefCell<DemoState> = RefCell::new(DemoState::new());
}

#[derive(Clone)]
struct PendingMessage {
    id: String,
    thread_id: String,
    text: String,
    action: String,
    status: String,
    polls: u8,
    attachments: Vec<Value>,
}

struct DemoState {
    messages: Vec<Value>,
    pending: Vec<PendingMessage>,
    active_thread: Option<String>,
    active_ticks: u8,
    active_scenario: Option<u8>,
    active_prompt: String,
    active_steers: Vec<String>,
    active_tool_message: Option<usize>,
    next_pending: u32,
    file_len: u64,
    model: String,
    effort: String,
    pinned_thread: Option<String>,
    renamed_threads: HashMap<String, String>,
}

impl DemoState {
    fn new() -> Self {
        Self {
            messages: seed_messages(),
            pending: Vec::new(),
            active_thread: None,
            active_ticks: 0,
            active_scenario: None,
            active_prompt: String::new(),
            active_steers: Vec::new(),
            active_tool_message: None,
            next_pending: 1,
            file_len: 48_320,
            model: "gpt-5.6-sol".to_owned(),
            effort: "medium".to_owned(),
            pinned_thread: Some(PRIMARY_THREAD.to_owned()),
            renamed_threads: HashMap::new(),
        }
    }

    fn thread(&self, id: &str) -> Value {
        let (default_title, created_at) = if id == SECONDARY_THREAD {
            ("Typed app-server tool items", "2026-09-06T09:38:46.147Z")
        } else {
            (
                "Build an interactive Pages demo",
                "2026-09-07T06:30:15.726Z",
            )
        };
        let title = self
            .renamed_threads
            .get(id)
            .map(String::as_str)
            .unwrap_or(default_title);
        json!({
            "id": id,
            "title": title,
            "cwd": PROJECT_PATH,
            "git_branch": "main",
            "created_at": created_at,
            "updated_at_ms": 1_788_767_541_844_u64,
            "source": "demo",
            "archived": false,
            "pinned": self.pinned_thread.as_deref() == Some(id),
        })
    }

    fn append_user_message(&mut self, text: String, attachments: Vec<Value>, action: &str) {
        let mut content = Vec::new();
        if !text.trim().is_empty() {
            content.push(json!({"kind": "text", "text": text}));
        }
        for attachment in attachments {
            let url = attachment
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default();
            content.push(json!({
                "kind": "context",
                "label": "Demo image attachment",
                "bytes": url.len(),
                "content": {"type": "input_image", "image_url": url},
            }));
        }
        self.messages.push(json!({
            "timestamp": "2026-09-07T07:00:00.000Z",
            "id": format!("demo-user-{}", self.messages.len()),
            "turn_id": "demo-turn-processing",
            "role": "user",
            "phase": null,
            "category": "user",
            "content": content,
            "tools": [],
            "demo_action": action,
        }));
    }

    fn scenario_for(text: &str, salt: u32) -> u8 {
        let lower = text.to_ascii_lowercase();
        if lower.contains("search") || lower.contains("research") || lower.contains("查找") {
            return 1;
        }
        if lower.contains("code") || lower.contains("fix") || lower.contains("代码") {
            return 2;
        }
        text.bytes()
            .fold(salt, |hash, byte| {
                hash.wrapping_mul(33).wrapping_add(byte as u32)
            })
            .wrapping_rem(3) as u8
    }

    fn scenario_tool(scenario: u8) -> &'static str {
        match scenario {
            1 => "web_search",
            2 => "apply_patch",
            _ => "exec_command",
        }
    }

    fn start_run(&mut self, pending: PendingMessage) {
        let scenario = Self::scenario_for(&pending.text, self.next_pending);
        self.append_user_message(pending.text.clone(), pending.attachments, "queue");
        self.active_thread = Some(pending.thread_id);
        self.active_ticks = 40;
        self.active_scenario = Some(scenario);
        self.active_prompt = pending.text;
        self.active_steers.clear();
        self.active_tool_message = None;
        self.file_len += 1;
    }

    fn apply_steer(&mut self, pending: PendingMessage) {
        self.append_user_message(pending.text.clone(), pending.attachments, "steer");
        if !pending.text.trim().is_empty() {
            self.active_steers.push(pending.text);
        }
        self.active_ticks = self.active_ticks.max(18);
        self.file_len += 1;
    }

    fn append_progress(&mut self) {
        let scenario = self.active_scenario.unwrap_or_default();
        let (text, preview) = match scenario {
            1 => (
                "I’m checking a few simulated sources and comparing the useful details.",
                "Search the demo knowledge index",
            ),
            2 => (
                "I found the relevant demo module and I’m preparing a small simulated patch.",
                "Update the live demo state machine",
            ),
            _ => (
                "I’m inspecting the simulated workspace before choosing the next step.",
                "Inspect the demo workspace",
            ),
        };
        let tool_name = Self::scenario_tool(scenario);
        let message_index = self.messages.len();
        self.messages.push(json!({
            "timestamp": "2026-09-08T08:00:02.000Z",
            "id": format!("demo-progress-{message_index}"),
            "turn_id": "demo-turn-processing",
            "role": "assistant",
            "phase": "commentary",
            "category": "assistant",
            "content": [{"kind": "text", "text": text}],
            "tools": [{
                "tool_index": 0,
                "name": tool_name,
                "status": "running",
                "preview": preview,
                "has_output": true,
                "bytes": 184,
                "additions": if scenario == 2 { 24 } else { 0 },
                "deletions": if scenario == 2 { 3 } else { 0 },
                "file_count": if scenario == 2 { 2 } else { 0 },
                "detail": {
                    "display_input": {"type": "demoAction", "title": preview, "prompt": self.active_prompt},
                    "tool": {"call_id": format!("demo-live-{message_index}"), "name": tool_name, "status": "running", "input": {"prompt": self.active_prompt}, "output": null}
                }
            }],
        }));
        self.active_tool_message = Some(message_index);
        self.file_len += 1;
    }

    fn finish_progress_tool(&mut self) {
        let Some(message_index) = self.active_tool_message else {
            return;
        };
        let Some(tool) = self.messages[message_index]["tools"]
            .as_array_mut()
            .and_then(|tools| tools.first_mut())
        else {
            return;
        };
        tool["status"] = json!("completed");
        tool["detail"]["tool"]["status"] = json!("completed");
        tool["detail"]["tool"]["output"] = json!({
            "result": "Simulated locally by the in-browser WASM demo server."
        });
        self.active_tool_message = None;
        self.file_len += 1;
    }

    fn cancel_progress_tool(&mut self) {
        let Some(message_index) = self.active_tool_message else {
            return;
        };
        let Some(tool) = self.messages[message_index]["tools"]
            .as_array_mut()
            .and_then(|tools| tools.first_mut())
        else {
            return;
        };
        tool["status"] = json!("cancelled");
        tool["detail"]["tool"]["status"] = json!("cancelled");
        tool["detail"]["tool"]["output"] = json!({
            "result": "Cancelled by the visitor in the live demo."
        });
        self.active_tool_message = None;
        self.file_len += 1;
    }

    fn append_demo_reply(&mut self) {
        let scenario = self.active_scenario.unwrap_or_default();
        let steer_note = if self.active_steers.is_empty() {
            String::new()
        } else {
            format!(
                " I also applied {} follow-up instruction{} while the run was active.",
                self.active_steers.len(),
                if self.active_steers.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        };
        let reply = match scenario {
            1 => "The simulated research pass is complete. The live demo selected a source-review template from your prompt and exercised the same progress refresh used by a real session.",
            2 => "The simulated code change is complete. The live demo selected an edit template, rendered a structured tool call, and refreshed this conclusion into the conversation.",
            _ => "The simulated workspace check is complete. This response was selected from a local template and no message or attachment left your browser.",
        };
        self.messages.push(json!({
            "timestamp": "2026-09-07T07:00:04.000Z",
            "id": format!("demo-assistant-{}", self.messages.len()),
            "turn_id": "demo-turn-processing",
            "role": "assistant",
            "phase": "final_answer",
            "category": "assistant",
            "content": [{
                "kind": "text",
                "text": format!("{reply}{steer_note}")
            }, {
                "kind": "memory_citation",
                "source": "MEMORY.md:47-70",
                "note": "demo workflow"
            }, {
                "kind": "turn_usage",
                "total_tokens": 18420,
                "input_tokens": 17680,
                "cached_input_tokens": 14336,
                "output_tokens": 740
            }],
            "tools": [],
        }));
    }

    fn pending_messages(&mut self) -> Value {
        let mut visible = self
            .pending
            .iter()
            .map(|pending| {
                json!({
                    "id": pending.id,
                    "thread_id": pending.thread_id,
                    "text": pending.text,
                    "action": pending.action,
                    "status": pending.status,
                    "source": "demo_wasm",
                })
            })
            .collect::<Vec<_>>();

        for pending in &mut self.pending {
            pending.polls += 1;
            if pending.polls == 1 {
                pending.status = if pending.action == "steer" {
                    "steered".to_owned()
                } else {
                    "queued".to_owned()
                };
            }
        }

        let ready_index = self
            .pending
            .iter()
            .position(|pending| {
                pending.polls >= 2
                    && pending.action == "steer"
                    && self.active_thread.as_deref() == Some(pending.thread_id.as_str())
            })
            .or_else(|| {
                if self.active_thread.is_none() {
                    self.pending
                        .iter()
                        .position(|pending| pending.polls >= 3 && pending.action == "queue")
                } else {
                    None
                }
            });
        if let Some(index) = ready_index {
            let pending = self.pending.remove(index);
            if pending.action == "steer" {
                self.apply_steer(pending);
            } else {
                self.start_run(pending);
            }
            let remaining_ids = self
                .pending
                .iter()
                .map(|pending| pending.id.as_str())
                .collect::<Vec<_>>();
            visible.retain(|entry| {
                entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| remaining_ids.contains(&id))
            });
        }

        json!({"messages": visible})
    }

    fn activity(&mut self, thread_id: &str) -> Value {
        let active_here = self.active_thread.as_deref() == Some(thread_id) && self.active_ticks > 0;
        if active_here {
            self.active_ticks -= 1;
            match self.active_ticks {
                38 => self.append_progress(),
                24 => self.finish_progress_tool(),
                10 => {
                    self.messages.push(json!({
                        "timestamp": "2026-09-08T08:00:05.000Z",
                        "id": format!("demo-analysis-{}", self.messages.len()),
                        "turn_id": "demo-turn-processing",
                        "role": "assistant",
                        "phase": "commentary",
                        "category": "assistant",
                        "content": [{"kind": "text", "text": "The simulated result is ready; I’m turning it into a concise final response."}],
                        "tools": [],
                    }));
                    self.file_len += 1;
                }
                _ => {}
            }
            if self.active_ticks == 0 {
                self.append_demo_reply();
                self.active_thread = None;
                self.active_scenario = None;
                self.active_tool_message = None;
                self.file_len += 1;
            }
        }
        let still_active =
            self.active_thread.as_deref() == Some(thread_id) && self.active_ticks > 0;
        json!({
            "thread_id": thread_id,
            "activity": {
                "active_turn_id": still_active.then_some("demo-turn-processing"),
                "phase": still_active.then_some(if self.active_tool_message.is_some() { "tool" } else { "model" }),
                "active_tool": still_active.then(|| self.active_scenario.map(Self::scenario_tool)).flatten(),
                "file_len": self.file_len,
                "updated_at_ms": 1_788_767_541_844_u64 + self.file_len,
            }
        })
    }
}

fn seed_messages() -> Vec<Value> {
    vec![
        json!({
            "timestamp": "2026-09-07T06:30:15.726Z",
            "id": "demo-user-0",
            "turn_id": "demo-turn-seed-1",
            "role": "user",
            "phase": null,
            "category": "user",
            "content": [{"kind": "text", "text": "Build a GitHub Pages demo using the same responsive Web UI."}],
            "tools": [],
        }),
        json!({
            "timestamp": "2026-09-07T06:30:21.000Z",
            "id": "demo-assistant-1",
            "turn_id": "demo-turn-seed-1",
            "role": "assistant",
            "phase": "commentary",
            "category": "assistant",
            "content": [{"kind": "text", "text": "I’ll keep the production frontend intact and replace only its command transport with a small in-browser demo server."}],
            "tools": [{
                "tool_index": 0,
                "name": "exec_command",
                "status": "completed",
                "preview": "cargo build --target wasm32-unknown-unknown",
                "has_output": true,
                "bytes": 284,
                "additions": null,
                "deletions": null,
                "file_count": null,
                "command_action_count": 2,
                "command_actions_parallel": false
            }],
        }),
        json!({
            "timestamp": "2026-09-07T06:31:04.000Z",
            "id": "demo-assistant-2",
            "turn_id": "demo-turn-seed-1",
            "role": "assistant",
            "phase": "commentary",
            "category": "assistant",
            "content": [{"kind": "text", "text": "The demo server now speaks the bridge request protocol and returns typed tool details."}],
            "tools": [{
                "tool_index": 0,
                "name": "apply_patch",
                "status": "completed",
                "preview": "已编辑 3 个文件",
                "has_output": true,
                "bytes": 1240,
                "additions": 86,
                "deletions": 4,
                "file_count": 3
            }],
        }),
        json!({
            "timestamp": "2026-09-07T06:31:40.000Z",
            "id": "demo-user-3",
            "turn_id": "demo-turn-seed-2",
            "role": "user",
            "phase": null,
            "category": "user",
            "content": [{"kind": "text", "text": "Can visitors try submitting a message without connecting to my Mac?"}],
            "tools": [],
        }),
        json!({
            "timestamp": "2026-09-07T06:31:46.000Z",
            "id": "demo-assistant-4",
            "turn_id": "demo-turn-seed-2",
            "role": "assistant",
            "phase": "final_answer",
            "category": "assistant",
            "content": [{"kind": "text", "text": "Yes. Try the composer below: the WASM state machine demonstrates submission, queue handoff, processing, and a simulated response entirely inside your browser."}, {"kind": "memory_citation", "source": "MEMORY.md:47-70", "note": "demo and deployment workflow"}, {"kind": "turn_usage", "total_tokens": 12480, "input_tokens": 11840, "cached_input_tokens": 9216, "output_tokens": 640}],
            "tools": [{
                "tool_index": 0,
                "name": "web_search",
                "status": "completed",
                "preview": "GitHub Pages WebAssembly MIME type",
                "has_output": true,
                "bytes": 376,
                "additions": null,
                "deletions": null,
                "file_count": null
            }],
        }),
    ]
}

fn models() -> Value {
    json!({"models": [
        {
            "id": "gpt-5.6-sol",
            "name": "GPT-5.6-Sol (demo)",
            "description": "A simulated model option for the Pages demo.",
            "default_effort": "medium",
            "efforts": [
                {"id": "low", "description": "Fast responses with lighter reasoning"},
                {"id": "medium", "description": "Balanced speed and reasoning depth"},
                {"id": "high", "description": "Greater reasoning depth"}
            ]
        },
        {
            "id": "gpt-5.4-mini",
            "name": "GPT-5.4-Mini (demo)",
            "description": "A compact simulated model option.",
            "default_effort": "low",
            "efforts": [
                {"id": "low", "description": "Fast demo responses"},
                {"id": "medium", "description": "More simulated reasoning"}
            ]
        }
    ]})
}

fn seed_tool_content(message_index: usize, tool_index: usize) -> Option<Value> {
    match (message_index, tool_index) {
        (1, 0) => Some(json!({
            "display_input": {
                "type": "commandExecution",
                "command": "cargo build --release --target wasm32-unknown-unknown -p codex-bridge-demo && npm run build",
                "commandActions": [
                    {"type": "build", "command": "cargo build --release --target wasm32-unknown-unknown \\\n  -p codex-bridge-demo", "path": "crates/codex-bridge-demo"},
                    {"type": "build", "command": "npm run build", "path": "web-ui"}
                ],
                "cwd": PROJECT_PATH
            },
            "tool": {"call_id": "demo-command", "name": "exec_command", "status": "completed", "input": {"type": "commandExecution"}, "output": {"exitCode": 0, "aggregatedOutput": "Finished release profile [optimized]"}}
        })),
        (2, 0) => Some(json!({
            "display_input": {"type": "fileChange", "changes": [
                {"path": format!("{PROJECT_PATH}/crates/codex-bridge-demo/src/lib.rs"), "kind": {"type": "add", "move_path": null}, "diff": "@@ -0,0 +1,5 @@\n+pub fn demo_command(request: &str) -> String {\n+    dispatch(request)\n+}\n"},
                {"path": format!("{PROJECT_PATH}/web-ui/src/api.js"), "kind": {"type": "update", "move_path": null}, "diff": "@@ -1,3 +1,5 @@\n+const demoMode = import.meta.env.VITE_CODEX_BRIDGE_DEMO === \\\"1\\\";\n export async function command(request) {\n"},
                {"path": format!("{PROJECT_PATH}/.github/workflows/pages.yml"), "kind": {"type": "update", "move_path": null}, "diff": "@@ -20,2 +20,4 @@\n+      - run: cargo build --target wasm32-unknown-unknown\n+      - run: npm run build\n"}
            ]},
            "tool": {"call_id": "demo-patch", "name": "apply_patch", "status": "completed", "input": {"type": "fileChange"}, "output": {"success": true}}
        })),
        (4, 0) => Some(json!({
            "display_input": {"type": "webSearch", "query": "GitHub Pages WebAssembly MIME type"},
            "tool": {"call_id": "demo-search", "name": "web_search", "status": "completed", "input": {"type": "webSearch", "query": "GitHub Pages WebAssembly MIME type"}, "output": {"result": "GitHub Pages serves .wasm assets for browser instantiation."}}
        })),
        _ => None,
    }
}

fn public_message(mut message: Value) -> Value {
    if let Some(tools) = message.get_mut("tools").and_then(Value::as_array_mut) {
        for tool in tools {
            if let Some(fields) = tool.as_object_mut() {
                fields.remove("detail");
            }
        }
    }
    message
}

fn live_tool_content(state: &DemoState, message_index: usize, tool_index: usize) -> Option<Value> {
    state
        .messages
        .get(message_index)?
        .get("tools")?
        .as_array()?
        .get(tool_index)?
        .get("detail")
        .cloned()
        .or_else(|| seed_tool_content(message_index, tool_index))
}

fn success(result: Value) -> Value {
    json!({"ok": true, "result": result})
}

fn error(code: &str, message: impl Into<String>) -> Value {
    json!({"ok": false, "error": {"code": code, "message": message.into()}})
}

fn dispatch(request: Value, state: &mut DemoState) -> Value {
    let Some(command) = request.get("command").and_then(Value::as_str) else {
        return error("invalid_request", "command must be a string");
    };
    let thread_id = request
        .get("thread_id")
        .and_then(Value::as_str)
        .unwrap_or(PRIMARY_THREAD);

    let result = match command {
        "status" => json!({
            "service": "codex-bridge-demo",
            "status": "ready",
            "demo": true,
            "live_simulation": true,
            "protocol_version": 23,
            "capabilities": {
                "audio_transcription": {
                    "enabled": true,
                    "reason": null,
                    "auth_mode": "demo",
                    "backend": "demo_wasm"
                }
            },
            "managed_services": {
                "app_server": {"enabled": true, "status": {"running": true, "restart_count": 0}},
                "desktop_interposition": {"enabled": false, "listen": null, "status": {"running": false, "restart_count": 0}},
                "whisper": {"enabled": false, "fallback": true, "needed": false, "listen": null, "status": {"running": false, "restart_count": 0}}
            },
            "rollout_store": {"available": true, "codex_home": "/demo/.codex", "read_only": true},
            "selected_thread_id": PRIMARY_THREAD,
            "write_backend": {"app_server_available": true, "app_server_mode": "demo_wasm", "standalone_fallback": false}
        }),
        "projects" => json!({
            "source": "demo_wasm",
            "include_archived": request.get("include_archived").and_then(Value::as_bool).unwrap_or(false),
            "projects": [
                {"name": "codex-bridge", "path": PROJECT_PATH, "kind": "project", "thread_count": 1, "archived_count": 0, "updated_at_ms": 1_788_767_541_844_u64},
                {"name": "Chats", "path": CHATS_PROJECT_PATH, "kind": "chats", "thread_count": 1, "archived_count": 0, "updated_at_ms": 1_788_767_500_000_u64}
            ]
        }),
        "project_threads" => {
            let project_path = request
                .get("project_path")
                .and_then(Value::as_str)
                .unwrap_or(PROJECT_PATH);
            let threads = if project_path == CHATS_PROJECT_PATH {
                vec![state.thread(SECONDARY_THREAD)]
            } else {
                vec![state.thread(PRIMARY_THREAD)]
            };
            json!({
                "source": "demo_wasm",
                "project_path": project_path,
                "threads": threads,
                "offset": 0,
                "returned": 1,
                "available": 1
            })
        }
        "thread_pins" => {
            let thread_ids = state.pinned_thread.iter().collect::<Vec<_>>();
            let threads = thread_ids
                .iter()
                .map(|thread_id| state.thread(thread_id))
                .collect::<Vec<_>>();
            json!({"available": true, "section_id": "demo-pins", "thread_ids": thread_ids, "threads": threads})
        }
        "thread_pin" => {
            let pinned = request
                .get("pinned")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if pinned {
                state.pinned_thread = Some(thread_id.to_owned());
            } else if state.pinned_thread.as_deref() == Some(thread_id) {
                state.pinned_thread = None;
            }
            json!({"thread_id": thread_id, "pinned": pinned})
        }
        "current" | "select" => {
            json!({"thread": state.thread(thread_id), "selection": {"method": "demo", "authoritative": true}})
        }
        "messages" => {
            let total = state.messages.len();
            let end = request
                .get("before")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(total)
                .min(total);
            let limit = request.get("limit").and_then(Value::as_u64).unwrap_or(30) as usize;
            let start = end.saturating_sub(limit);
            let messages = state.messages[start..end]
                .iter()
                .enumerate()
                .map(|(offset, message)| {
                    let mut message = public_message(message.clone());
                    message["message_index"] = json!(start + offset);
                    message
                })
                .collect::<Vec<_>>();
            json!({
                "source": "demo_wasm",
                "tool_source": "demo_wasm",
                "thread": state.thread(thread_id),
                "messages": messages,
                "page": {"start": start, "end": end, "total": total, "has_more": start > 0, "before": start}
            })
        }
        "tool_content" => {
            let message_index = request
                .get("message_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let tool_index = request
                .get("tool_index")
                .and_then(Value::as_u64)
                .unwrap_or(0) as usize;
            let Some(mut detail) = live_tool_content(state, message_index, tool_index) else {
                return error(
                    "tool_content_not_found",
                    "the demo has no detail for this tool",
                );
            };
            detail["thread_id"] = json!(thread_id);
            detail["message_index"] = json!(message_index);
            detail["tool_index"] = json!(tool_index);
            detail
        }
        "thread_watch" => json!({"thread_id": thread_id, "subscribed": true}),
        "pending_messages" => state.pending_messages(),
        "pending_message_delete" => {
            let requested_id = request
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let Some(index) = state
                .pending
                .iter()
                .position(|pending| pending.id == requested_id && pending.thread_id == thread_id)
            else {
                return error(
                    "pending_message_not_found",
                    "the demo message already handed off",
                );
            };
            let pending = state.pending.remove(index);
            json!({"id": pending.id, "thread_id": pending.thread_id, "text": pending.text, "message_action": pending.action, "queue_deleted": true})
        }
        "send" | "steer" => {
            let text = request
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            let attachments = request
                .get("attachments")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if attachments
                .iter()
                .any(|attachment| attachment.get("type").and_then(Value::as_str) == Some("audio"))
            {
                return error(
                    "invalid_attachment",
                    "audio must be transcribed before demo message submission",
                );
            }
            if text.is_empty() && attachments.is_empty() {
                return error(
                    "invalid_request",
                    "message must contain text or an attachment",
                );
            }
            if command == "steer"
                && (state.active_thread.as_deref() != Some(thread_id) || state.active_ticks == 0)
            {
                return error("no_active_turn", "start a demo run before steering it");
            }
            let action = if command == "steer" { "steer" } else { "queue" };
            let id = format!("demo-pending-{}", state.next_pending);
            state.next_pending += 1;
            let summary = if text.is_empty() {
                "Demo image message".to_owned()
            } else {
                text.to_owned()
            };
            state.pending.push(PendingMessage {
                id: id.clone(),
                thread_id: thread_id.to_owned(),
                text: summary,
                action: action.to_owned(),
                status: format!("{action}ing"),
                polls: 0,
                attachments,
            });
            json!({"action": command, "status": if command == "steer" { "steered" } else { "queued" }, "pending_id": id, "thread_id": thread_id, "target": "demo_wasm"})
        }
        "thread_activity" => state.activity(thread_id),
        "interrupt" => {
            if state.active_thread.as_deref() != Some(thread_id) || state.active_ticks == 0 {
                return error(
                    "no_active_turn",
                    format!("thread {thread_id} has no active demo turn"),
                );
            }
            state.cancel_progress_tool();
            state.active_ticks = 0;
            state.active_thread = None;
            state.active_scenario = None;
            state.active_tool_message = None;
            state.messages.push(json!({
                "timestamp": "2026-09-08T08:00:06.000Z",
                "id": format!("demo-cancelled-{}", state.messages.len()),
                "turn_id": "demo-turn-processing",
                "role": "assistant",
                "phase": "final_answer",
                "category": "assistant",
                "content": [{"kind": "text", "text": "Demo run cancelled. Any queued message remains available to withdraw or run next."}],
                "tools": [],
            }));
            state.file_len += 1;
            json!({"thread_id": thread_id, "status": "interrupted", "backend": "demo_wasm"})
        }
        "composer_status" => json!({
            "thread_id": thread_id,
            "model": state.model,
            "reasoning_effort": state.effort,
            "weekly_usage": {"remaining_percent": 63, "resets_at": 1_790_000_000_u64}
        }),
        "composer_options" => models(),
        "audio_transcribe" => {
            let audio = request.get("audio").and_then(Value::as_object);
            if audio
                .and_then(|value| value.get("data"))
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                return error("invalid_audio", "the demo transcription requires PCM audio");
            }
            json!({
                "action": "audio_transcribe",
                "thread_id": thread_id,
                "text": "Please summarize the current implementation and keep the answer concise.",
                "backend": "demo_wasm"
            })
        }
        "thread_settings_update" => {
            state.model = request
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("gpt-5.6-sol")
                .to_owned();
            state.effort = request
                .get("effort")
                .and_then(Value::as_str)
                .unwrap_or("medium")
                .to_owned();
            json!({"thread_id": thread_id, "model": state.model, "reasoning_effort": state.effort, "backend": "demo_wasm"})
        }
        "thread_rename" => {
            let name = request
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or("");
            if name.is_empty() || name.chars().count() > 200 {
                return error(
                    "invalid_thread_name",
                    "thread name must contain between 1 and 200 characters",
                );
            }
            state
                .renamed_threads
                .insert(thread_id.to_owned(), name.to_owned());
            state.file_len += 1;
            json!({"thread_id": thread_id, "name": name, "status": "renamed", "backend": "demo_wasm"})
        }
        "workspace_diff" => json!({
            "thread_id": thread_id,
            "repository": PROJECT_PATH,
            "base_branch": "main",
            "base_sha": "da7e084d14eccd6cac359726d802e78ddd1f54de",
            "semantics": "demo_uncommitted_worktree_vs_head",
            "clean": false,
            "files_changed": 4,
            "additions": 214,
            "deletions": 18,
            "untracked_files": 1,
            "untracked_lines_skipped": 0
        }),
        "tail" => {
            json!({"source": "demo_wasm", "messages": state.messages.iter().cloned().map(public_message).collect::<Vec<_>>(), "messages_total": state.messages.len()})
        }
        "pending" => json!({"requests": []}),
        "scroll" => json!({"status": "simulated", "direction": request.get("direction")}),
        "message_content" => {
            return error(
                "message_content_not_found",
                "the demo messages are already fully loaded",
            )
        }
        "thread_create" | "thread_archive" | "approve" | "decline" | "host_exec"
        | "app_server_rpc" => {
            return error(
                "demo_limited",
                "This operation is intentionally unavailable in the public WASM demo",
            )
        }
        _ => {
            return error(
                "unknown_command",
                format!("unsupported demo command: {command}"),
            )
        }
    };
    success(result)
}

pub fn handle_json(request: &str) -> String {
    let response = match serde_json::from_str::<Value>(request) {
        Ok(request) => STATE.with(|state| dispatch(request, &mut state.borrow_mut())),
        Err(parse_error) => error("invalid_json", parse_error.to_string()),
    };
    serde_json::to_string(&response).expect("demo response serializes")
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub extern "C" fn demo_alloc(len: usize) -> *mut u8 {
    let mut bytes = vec![0_u8; len].into_boxed_slice();
    let pointer = bytes.as_mut_ptr();
    std::mem::forget(bytes);
    pointer
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn demo_free(pointer: *mut u8, len: usize) {
    if !pointer.is_null() {
        // SAFETY: the pointer and exact length come from `demo_alloc` or `demo_command`.
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            pointer, len,
        )));
    }
}

#[cfg(target_arch = "wasm32")]
#[no_mangle]
pub unsafe extern "C" fn demo_command(pointer: *const u8, len: usize) -> u64 {
    // SAFETY: JavaScript writes `len` initialized bytes into a `demo_alloc` allocation.
    let request = std::slice::from_raw_parts(pointer, len);
    let request = std::str::from_utf8(request).unwrap_or("{}");
    let mut response = handle_json(request).into_bytes().into_boxed_slice();
    let response_len = response.len() as u64;
    let response_pointer = response.as_mut_ptr() as usize as u64;
    std::mem::forget(response);
    (response_len << 32) | (response_pointer & 0xffff_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(response: Value) -> Value {
        assert_eq!(response["ok"], true, "unexpected demo response: {response}");
        response["result"].clone()
    }

    fn land_pending_message(state: &mut DemoState) {
        for _ in 0..3 {
            result(dispatch(json!({"command": "pending_messages"}), state));
        }
    }

    fn finish_active_run(state: &mut DemoState) {
        while state.active_ticks > 0 {
            result(dispatch(
                json!({"command": "thread_activity", "thread_id": PRIMARY_THREAD}),
                state,
            ));
        }
    }

    #[test]
    fn status_identifies_the_wasm_demo() {
        let response: Value =
            serde_json::from_str(&handle_json(r#"{"command":"status"}"#)).unwrap();
        assert_eq!(response["result"]["demo"], true);
        assert_eq!(response["result"]["live_simulation"], true);
        assert_eq!(response["result"]["protocol_version"], 23);
    }

    #[test]
    fn messages_have_exact_page_indices() {
        let mut state = DemoState::new();
        let response = dispatch(
            json!({"command": "messages", "thread_id": PRIMARY_THREAD, "limit": 2}),
            &mut state,
        );
        assert_eq!(response["result"]["page"]["start"], 3);
        assert_eq!(response["result"]["page"]["end"], 5);
        assert_eq!(response["result"]["messages"][0]["message_index"], 3);
    }

    #[test]
    fn queued_message_moves_through_buffer_and_into_history() {
        let mut state = DemoState::new();
        let before = state.messages.len();
        let response = result(dispatch(
            json!({"command": "send", "thread_id": PRIMARY_THREAD, "text": "Try the demo"}),
            &mut state,
        ));
        assert_eq!(response["status"], "queued");
        let pending = state.pending_messages();
        assert_eq!(pending["messages"][0]["status"], "queueing");
        assert_eq!(pending["messages"][0]["action"], "queue");
        assert_eq!(state.pending_messages()["messages"][0]["status"], "queued");
        assert!(state.pending_messages()["messages"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(state.messages.len(), before + 1);
        assert_eq!(state.active_ticks, 40);
    }

    #[test]
    fn polling_refreshes_activity_and_lands_the_demo_reply() {
        let mut state = DemoState::new();
        let initial_messages = state.messages.len();
        let initial_file_len = state.file_len;
        result(dispatch(
            json!({"command": "send", "thread_id": PRIMARY_THREAD, "text": "Refresh me"}),
            &mut state,
        ));
        land_pending_message(&mut state);

        let active = result(dispatch(
            json!({"command": "thread_activity", "thread_id": PRIMARY_THREAD}),
            &mut state,
        ));
        assert_eq!(active["activity"]["active_turn_id"], "demo-turn-processing");
        assert!(active["activity"]["file_len"].as_u64().unwrap() > initial_file_len);

        finish_active_run(&mut state);
        let completed = result(dispatch(
            json!({"command": "thread_activity", "thread_id": PRIMARY_THREAD}),
            &mut state,
        ));
        assert!(completed["activity"]["active_turn_id"].is_null());

        let messages = result(dispatch(
            json!({"command": "messages", "thread_id": PRIMARY_THREAD, "limit": 30}),
            &mut state,
        ));
        assert_eq!(messages["page"]["total"], initial_messages + 4);
        assert_eq!(
            messages["messages"][initial_messages]["content"][0]["text"],
            "Refresh me"
        );
        assert!(
            messages["messages"].as_array().unwrap().last().unwrap()["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("simulated")
        );
    }

    #[test]
    fn active_demo_turn_can_be_interrupted_once() {
        let mut state = DemoState::new();
        result(dispatch(
            json!({"command": "send", "thread_id": PRIMARY_THREAD, "text": "Stop me"}),
            &mut state,
        ));
        land_pending_message(&mut state);
        result(dispatch(
            json!({"command": "thread_activity", "thread_id": PRIMARY_THREAD}),
            &mut state,
        ));
        result(dispatch(
            json!({"command": "thread_activity", "thread_id": PRIMARY_THREAD}),
            &mut state,
        ));
        let messages_after_handoff = state.messages.len();
        let file_len_before_interrupt = state.file_len;

        let interrupted = result(dispatch(
            json!({"command": "interrupt", "thread_id": PRIMARY_THREAD}),
            &mut state,
        ));
        assert_eq!(interrupted["status"], "interrupted");
        assert_eq!(state.active_thread, None);
        assert_eq!(state.active_ticks, 0);
        assert_eq!(state.messages.len(), messages_after_handoff + 1);
        assert_eq!(
            state.messages[messages_after_handoff - 1]["tools"][0]["status"],
            "cancelled"
        );
        assert!(state.messages.last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cancelled"));
        assert!(state.file_len > file_len_before_interrupt);

        let inactive = result(dispatch(
            json!({"command": "thread_activity", "thread_id": PRIMARY_THREAD}),
            &mut state,
        ));
        assert!(inactive["activity"]["active_turn_id"].is_null());
        let duplicate = dispatch(
            json!({"command": "interrupt", "thread_id": PRIMARY_THREAD}),
            &mut state,
        );
        assert_eq!(duplicate["ok"], false);
        assert_eq!(duplicate["error"]["code"], "no_active_turn");
    }

    #[test]
    fn active_run_accepts_steer_and_keeps_queued_work_withdrawable() {
        let mut state = DemoState::new();
        result(dispatch(
            json!({"command": "send", "thread_id": PRIMARY_THREAD, "text": "Fix the demo code"}),
            &mut state,
        ));
        land_pending_message(&mut state);

        let queued = result(dispatch(
            json!({"command": "send", "thread_id": PRIMARY_THREAD, "text": "Then summarize it"}),
            &mut state,
        ));
        let steer = result(dispatch(
            json!({"command": "steer", "thread_id": PRIMARY_THREAD, "text": "Keep the answer short"}),
            &mut state,
        ));
        result(dispatch(json!({"command": "pending_messages"}), &mut state));
        let pending = result(dispatch(json!({"command": "pending_messages"}), &mut state));
        assert_eq!(pending["messages"].as_array().unwrap().len(), 1);
        assert_eq!(pending["messages"][0]["action"], "queue");
        assert_eq!(state.active_steers, vec!["Keep the answer short"]);

        let withdrawn = result(dispatch(
            json!({"command": "pending_message_delete", "thread_id": PRIMARY_THREAD, "id": queued["pending_id"]}),
            &mut state,
        ));
        assert_eq!(withdrawn["queue_deleted"], true);
        assert_eq!(withdrawn["text"], "Then summarize it");
        assert_ne!(queued["pending_id"], steer["pending_id"]);

        finish_active_run(&mut state);
        assert!(state.messages.last().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("follow-up instruction"));
    }

    #[test]
    fn audio_transcription_returns_editable_text_without_submitting_a_message() {
        let mut state = DemoState::new();
        let before = state.messages.len();
        let transcription = result(dispatch(
            json!({
                "command": "audio_transcribe",
                "thread_id": PRIMARY_THREAD,
                "audio": {"data": "AAAAAA==", "sample_rate": 24000, "num_channels": 1, "samples_per_channel": 2}
            }),
            &mut state,
        ));
        assert_eq!(state.messages.len(), before);
        assert_eq!(transcription["backend"], "demo_wasm");
        assert!(transcription["text"]
            .as_str()
            .unwrap()
            .contains("summarize"));

        let attachment = dispatch(
            json!({
                "command": "send",
                "thread_id": PRIMARY_THREAD,
                "text": "",
                "attachments": [{"type": "audio", "url": "data:audio/wav;base64,UklGRg=="}]
            }),
            &mut state,
        );
        assert_eq!(attachment["ok"], false);
        assert_eq!(attachment["error"]["code"], "invalid_attachment");
    }

    #[test]
    fn demo_exposes_the_frontend_baseline_contract() {
        let mut state = DemoState::new();
        for (request, field) in [
            (json!({"command": "status"}), "service"),
            (json!({"command": "projects"}), "projects"),
            (
                json!({"command": "project_threads", "project_path": PROJECT_PATH}),
                "threads",
            ),
            (json!({"command": "composer_options"}), "models"),
            (
                json!({"command": "workspace_diff", "thread_id": PRIMARY_THREAD}),
                "files_changed",
            ),
        ] {
            assert!(result(dispatch(request, &mut state)).get(field).is_some());
        }
    }

    #[test]
    fn pinning_tracks_the_selected_demo_thread() {
        let mut state = DemoState::new();
        dispatch(
            json!({"command": "thread_pin", "thread_id": SECONDARY_THREAD, "pinned": true}),
            &mut state,
        );
        let pins = dispatch(json!({"command": "thread_pins"}), &mut state);
        assert_eq!(pins["result"]["thread_ids"], json!([SECONDARY_THREAD]));
        assert_eq!(pins["result"]["threads"][0]["id"], SECONDARY_THREAD);
        assert_eq!(state.thread(PRIMARY_THREAD)["pinned"], false);
        assert_eq!(state.thread(SECONDARY_THREAD)["pinned"], true);
    }
}
