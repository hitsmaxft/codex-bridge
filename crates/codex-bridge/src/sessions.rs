use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CODEX_HOME_ENV: &str = "CODEX_HOME";

#[derive(Debug, Clone)]
pub struct SessionStore {
    codex_home: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadSummary {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub cwd: PathBuf,
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub updated_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub archived: bool,
    pub rollout_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadSnapshot {
    pub thread: ThreadSummary,
    pub messages: Vec<ThreadMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    pub content: Vec<Value>,
}

#[derive(Debug, Deserialize)]
struct SessionIndexEntry {
    id: String,
    thread_name: String,
    updated_at: String,
}

pub fn default_codex_home() -> Result<PathBuf> {
    if let Some(path) = env::var_os(CODEX_HOME_ENV).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }

    let home = env::var_os("HOME").context("HOME is not set; pass --codex-home explicitly")?;
    Ok(PathBuf::from(home).join(".codex"))
}

impl SessionStore {
    pub fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    pub fn home(&self) -> &Path {
        &self.codex_home
    }

    pub fn is_available(&self) -> bool {
        self.codex_home.join("sessions").is_dir()
    }

    pub fn list_threads(&self, include_archived: bool) -> Result<Vec<ThreadSummary>> {
        let mut threads = self.scan_threads(include_archived)?;
        populate_git_branches(&mut threads);
        Ok(threads)
    }

    pub fn list_threads_limited(
        &self,
        include_archived: bool,
        limit: usize,
    ) -> Result<(Vec<ThreadSummary>, usize)> {
        let mut threads = self.scan_threads(include_archived)?;
        let available = threads.len();
        threads.truncate(limit);
        populate_git_branches(&mut threads);
        Ok((threads, available))
    }

    pub fn current_thread(&self) -> Result<Option<ThreadSummary>> {
        Ok(self.list_threads_limited(false, 1)?.0.into_iter().next())
    }

    pub fn find_thread(&self, thread_id: &str) -> Result<Option<ThreadSummary>> {
        let mut thread = self
            .scan_threads(true)?
            .into_iter()
            .find(|thread| thread.id == thread_id);
        if let Some(thread) = &mut thread {
            thread.git_branch = git_branch_for_cwd(&thread.cwd);
        }
        Ok(thread)
    }

    fn scan_threads(&self, include_archived: bool) -> Result<Vec<ThreadSummary>> {
        let titles = self.read_title_index()?;
        let mut files = Vec::new();
        collect_rollout_files(&self.codex_home.join("sessions"), false, &mut files)?;
        if include_archived {
            collect_rollout_files(&self.codex_home.join("archived_sessions"), true, &mut files)?;
        }

        let mut by_id = HashMap::<String, ThreadSummary>::new();
        for (path, archived) in files {
            let Some(summary) = read_rollout_summary(&path, archived, &titles)? else {
                continue;
            };

            match by_id.get(&summary.id) {
                Some(existing) if existing.updated_at_ms >= summary.updated_at_ms => {}
                _ => {
                    by_id.insert(summary.id.clone(), summary);
                }
            }
        }

        let mut threads: Vec<_> = by_id.into_values().collect();
        threads.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(threads)
    }

    pub fn read_thread(&self, thread_id: &str) -> Result<Option<ThreadSnapshot>> {
        let summary = self.find_thread(thread_id)?;
        let Some(summary) = summary else {
            return Ok(None);
        };
        let messages = read_rollout_messages(&summary.rollout_path)?;
        Ok(Some(ThreadSnapshot {
            thread: summary,
            messages,
        }))
    }

    pub fn active_turn_id(&self, thread_id: &str) -> Result<Option<String>> {
        let Some(summary) = self.find_thread(thread_id)? else {
            return Ok(None);
        };
        read_active_turn_id(&summary.rollout_path)
    }

    fn read_title_index(&self) -> Result<HashMap<String, String>> {
        let path = self.codex_home.join("session_index.jsonl");
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to open {}", path.display()));
            }
        };

        let mut latest = HashMap::<String, (String, String)>::new();
        for line in BufReader::new(file).lines() {
            let line = match line {
                Ok(line) => line,
                Err(error) if error.kind() == ErrorKind::InvalidData => continue,
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("failed to read {}", path.display()));
                }
            };
            let Ok(entry) = serde_json::from_str::<SessionIndexEntry>(&line) else {
                continue;
            };

            match latest.get(&entry.id) {
                Some((updated_at, _)) if updated_at >= &entry.updated_at => {}
                _ => {
                    latest.insert(entry.id, (entry.updated_at, entry.thread_name));
                }
            }
        }

        Ok(latest
            .into_iter()
            .map(|(id, (_, title))| (id, title))
            .collect())
    }
}

fn collect_rollout_files(
    root: &Path,
    archived: bool,
    output: &mut Vec<(PathBuf, bool)>,
) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", root.display()));
        }
    };

    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read entry in {}", root.display()))?;
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        let path = entry.path();
        if file_type.is_dir() {
            collect_rollout_files(&path, archived, output)?;
        } else if file_type.is_file() && path.extension().is_some_and(|value| value == "jsonl") {
            output.push((path, archived));
        }
    }
    Ok(())
}

fn read_rollout_summary(
    path: &Path,
    archived: bool,
    titles: &HashMap<String, String>,
) -> Result<Option<ThreadSummary>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut id = None;
    let mut cwd = None;
    let mut created_at = None;
    let mut source = None;
    let mut fallback_title = None;

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            // A live rollout can end in a partially written UTF-8 sequence. The
            // already completed records remain usable.
            Err(error) if error.kind() == ErrorKind::InvalidData => break,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        if record.get("type").and_then(Value::as_str) == Some("session_meta") {
            let payload = &record["payload"];
            id = string_field(payload, "id").or_else(|| string_field(payload, "session_id"));
            cwd = string_field(payload, "cwd").map(PathBuf::from);
            created_at =
                string_field(payload, "timestamp").or_else(|| string_field(&record, "timestamp"));
            source = string_field(payload, "source");

            if id.as_ref().is_some_and(|id| titles.contains_key(id)) {
                break;
            }
        } else if fallback_title.is_none() {
            fallback_title = first_user_message_title(&record);
        }

        if id.is_some() && fallback_title.is_some() {
            break;
        }
    }

    let Some(id) = id else {
        return Ok(None);
    };
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    let updated_at_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);

    Ok(Some(ThreadSummary {
        title: titles.get(&id).cloned().or(fallback_title),
        id,
        cwd,
        git_branch: None,
        created_at,
        updated_at_ms,
        source,
        archived,
        rollout_path: path.to_path_buf(),
    }))
}

fn populate_git_branches(threads: &mut [ThreadSummary]) {
    let mut by_cwd = HashMap::<PathBuf, Option<String>>::new();
    for thread in threads {
        thread.git_branch = by_cwd
            .entry(thread.cwd.clone())
            .or_insert_with(|| git_branch_for_cwd(&thread.cwd))
            .clone();
    }
}

fn git_branch_for_cwd(cwd: &Path) -> Option<String> {
    if !cwd.is_dir() {
        return None;
    }
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !output.status.success() || output.stdout.len() > 4 * 1024 {
        return None;
    }
    let branch = std::str::from_utf8(&output.stdout).ok()?.trim();
    (!branch.is_empty()).then(|| branch.to_owned())
}

fn read_rollout_messages(path: &Path) -> Result<Vec<ThreadMessage>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut messages = Vec::new();
    let mut seen_ids = HashSet::new();

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == ErrorKind::InvalidData => break,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            // The final line of an actively written rollout can be incomplete.
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }

        let payload = &record["payload"];
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(Value::as_str) else {
            continue;
        };
        if !matches!(role, "user" | "assistant") {
            continue;
        }

        let id = string_field(payload, "id");
        if id.as_ref().is_some_and(|id| !seen_ids.insert(id.clone())) {
            continue;
        }
        let Some(content) = payload.get("content").and_then(Value::as_array) else {
            continue;
        };
        messages.push(ThreadMessage {
            timestamp: string_field(&record, "timestamp"),
            id,
            role: role.to_owned(),
            phase: string_field(payload, "phase"),
            content: content.clone(),
        });
    }

    Ok(messages)
}

fn read_active_turn_id(path: &Path) -> Result<Option<String>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut active_turn_id = None;

    for line in BufReader::new(file).lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) if error.kind() == ErrorKind::InvalidData => break,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }

        let payload = &record["payload"];
        let event_type = payload.get("type").and_then(Value::as_str);
        let turn_id = payload.get("turn_id").and_then(Value::as_str);
        match event_type {
            Some("task_started") => active_turn_id = turn_id.map(str::to_owned),
            Some("task_complete" | "turn_aborted")
                if turn_id.is_none() || turn_id == active_turn_id.as_deref() =>
            {
                active_turn_id = None;
            }
            _ => {}
        }
    }

    Ok(active_turn_id)
}

fn first_user_message_title(record: &Value) -> Option<String> {
    if record.get("type").and_then(Value::as_str) != Some("response_item") {
        return None;
    }
    let payload = &record["payload"];
    if payload.get("type").and_then(Value::as_str) != Some("message")
        || payload.get("role").and_then(Value::as_str) != Some("user")
    {
        return None;
    }

    let text = payload
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(120).collect())
    }
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "codex-bridge-session-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(path.join("sessions/2026/08/30")).unwrap();
            Self { path }
        }

        fn write_rollout(&self, name: &str, records: &[&str]) -> PathBuf {
            let path = self.path.join("sessions/2026/08/30").join(name);
            let mut file = File::create(&path).unwrap();
            for record in records {
                writeln!(file, "{record}").unwrap();
            }
            path
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn init_git_repo(path: &Path, branch: &str) {
        fs::create_dir_all(path).unwrap();
        let status = Command::new("git")
            .args(["init", "--quiet", "--initial-branch", branch])
            .current_dir(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn reads_titles_and_user_assistant_messages_without_internal_records() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-example.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-1","timestamp":"2026-08-30T01:00:00Z","cwd":"/tmp/project","source":"vscode"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"response_item","payload":{"type":"message","id":"developer-1","role":"developer","content":[{"type":"input_text","text":"internal"}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"response_item","payload":{"type":"message","id":"user-1","role":"user","content":[{"type":"input_text","text":"hello world"}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:03Z","type":"response_item","payload":{"type":"message","id":"assistant-1","role":"assistant","phase":"final","content":[{"type":"output_text","text":"done"}]}}"#,
                "{incomplete",
            ],
        );
        fs::write(
            fixture.path.join("session_index.jsonl"),
            concat!(
                r#"{"id":"thread-1","thread_name":"Old title","updated_at":"2026-08-30T01:00:00Z"}"#,
                "\n",
                r#"{"id":"thread-1","thread_name":"Latest title","updated_at":"2026-08-30T02:00:00Z"}"#,
                "\n"
            ),
        )
        .unwrap();

        let store = SessionStore::new(fixture.path.clone());
        let threads = store.list_threads(false).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].title.as_deref(), Some("Latest title"));
        assert_eq!(threads[0].cwd, Path::new("/tmp/project"));
        assert_eq!(threads[0].git_branch, None);
        let thread_json = serde_json::to_value(&threads[0]).unwrap();
        assert_eq!(thread_json["cwd"], "/tmp/project");
        assert!(thread_json["git_branch"].is_null());

        let snapshot = store.read_thread("thread-1").unwrap().unwrap();
        assert_eq!(snapshot.messages.len(), 2);
        assert_eq!(snapshot.messages[0].role, "user");
        assert_eq!(snapshot.messages[1].role, "assistant");
        assert_eq!(snapshot.thread.cwd, Path::new("/tmp/project"));
        assert_eq!(snapshot.thread.git_branch, None);
    }

    #[test]
    fn session_list_and_detail_include_the_branch_from_the_rollout_cwd() {
        let fixture = Fixture::new();
        let workspace = fixture.path.join("workspace");
        init_git_repo(&workspace, "fixture-branch");
        let cwd = serde_json::to_string(&workspace).unwrap();
        let session_meta = format!(
            r#"{{"timestamp":"2026-08-31T01:00:00Z","type":"session_meta","payload":{{"id":"thread-git","timestamp":"2026-08-31T01:00:00Z","cwd":{cwd},"source":"fixture"}}}}"#
        );
        fixture.write_rollout("rollout-git.jsonl", &[&session_meta]);

        let store = SessionStore::new(fixture.path.clone());
        let (threads, available) = store.list_threads_limited(false, 1).unwrap();
        assert_eq!(available, 1);
        assert_eq!(threads[0].cwd, workspace);
        assert_eq!(threads[0].git_branch.as_deref(), Some("fixture-branch"));
        let thread_json = serde_json::to_value(&threads[0]).unwrap();
        assert_eq!(thread_json["git_branch"], "fixture-branch");

        let snapshot = store.read_thread("thread-git").unwrap().unwrap();
        assert_eq!(snapshot.thread.cwd, threads[0].cwd);
        assert_eq!(
            snapshot.thread.git_branch.as_deref(),
            Some("fixture-branch")
        );
    }

    #[test]
    fn git_branch_is_null_for_non_git_or_deleted_workspaces() {
        let fixture = Fixture::new();
        let non_git = fixture.path.join("non-git");
        fs::create_dir_all(&non_git).unwrap();

        assert_eq!(git_branch_for_cwd(&non_git), None);
        assert_eq!(git_branch_for_cwd(&fixture.path.join("deleted")), None);
    }

    #[test]
    fn missing_store_is_an_empty_read_only_source() {
        let fixture = Fixture::new();
        let missing = fixture.path.join("does-not-exist");
        let store = SessionStore::new(missing);

        assert!(!store.is_available());
        assert!(store.list_threads(false).unwrap().is_empty());
        assert!(store.current_thread().unwrap().is_none());
    }

    #[test]
    fn tracks_only_an_unfinished_latest_turn_as_active() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-active.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-active","timestamp":"2026-08-30T01:00:00Z","cwd":"/tmp/project","source":"vscode"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-old"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-old"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:03Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-active"}}"#,
            ],
        );

        let store = SessionStore::new(fixture.path.clone());
        assert_eq!(
            store.active_turn_id("thread-active").unwrap().as_deref(),
            Some("turn-active")
        );

        let path = fixture
            .path
            .join("sessions/2026/08/30/rollout-active.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(
            br#"{"timestamp":"2026-08-30T01:00:04Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-active"}}
"#,
        )
        .unwrap();
        assert_eq!(store.active_turn_id("thread-active").unwrap(), None);
    }
}
