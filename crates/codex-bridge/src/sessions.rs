use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CODEX_HOME_ENV: &str = "CODEX_HOME";
const GIT_BRANCH_CACHE_TTL: Duration = Duration::from_secs(30);
const GIT_BRANCH_PARALLELISM: usize = 16;
const SUMMARY_CACHE_TTL: Duration = Duration::from_secs(3);
const MESSAGE_CACHE_ENTRIES: usize = 4;

type GitBranchCache = Arc<Mutex<HashMap<PathBuf, (Instant, Option<String>)>>>;

#[derive(Debug, Clone)]
struct SummaryCache {
    refreshed_at: Instant,
    threads: Vec<ThreadSummary>,
}

#[derive(Debug, Clone)]
struct CachedMessages {
    modified: Option<SystemTime>,
    file_len: u64,
    used_at: Instant,
    messages: Arc<Vec<ThreadMessage>>,
}

#[derive(Debug, Clone)]
struct CachedActivity {
    processed_len: u64,
    active_turn_id: Option<String>,
    active_tools: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    codex_home: PathBuf,
    git_branch_cache: GitBranchCache,
    summary_cache: Arc<Mutex<Option<SummaryCache>>>,
    message_cache: Arc<Mutex<HashMap<PathBuf, CachedMessages>>>,
    activity_cache: Arc<Mutex<HashMap<PathBuf, CachedActivity>>>,
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ThreadToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ThreadToolCall {
    pub call_id: String,
    pub name: String,
    pub status: String,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectSummary {
    pub path: PathBuf,
    pub name: String,
    pub thread_count: usize,
    pub archived_count: usize,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectThreadSummary {
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
    #[serde(default)]
    pub pinned: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MessagePage {
    pub messages: Vec<ThreadMessage>,
    pub start: usize,
    pub end: usize,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThreadActivity {
    pub file_len: u64,
    pub updated_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_tool: Option<String>,
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
        Self {
            codex_home,
            git_branch_cache: Arc::new(Mutex::new(HashMap::new())),
            summary_cache: Arc::new(Mutex::new(None)),
            message_cache: Arc::new(Mutex::new(HashMap::new())),
            activity_cache: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn home(&self) -> &Path {
        &self.codex_home
    }

    pub fn is_available(&self) -> bool {
        self.codex_home.join("sessions").is_dir()
    }

    pub fn invalidate_summary_cache(&self) {
        if let Ok(mut cache) = self.summary_cache.lock() {
            *cache = None;
        }
    }

    pub fn list_threads(&self, include_archived: bool) -> Result<Vec<ThreadSummary>> {
        let mut threads = self.cached_threads(include_archived)?;
        populate_git_branches(&mut threads, &self.git_branch_cache);
        Ok(threads)
    }

    pub fn list_threads_limited(
        &self,
        include_archived: bool,
        limit: usize,
    ) -> Result<(Vec<ThreadSummary>, usize)> {
        let mut threads = self.cached_threads(include_archived)?;
        let available = threads.len();
        threads.truncate(limit);
        populate_git_branches(&mut threads, &self.git_branch_cache);
        Ok((threads, available))
    }

    pub fn current_thread(&self) -> Result<Option<ThreadSummary>> {
        Ok(self.list_threads_limited(false, 1)?.0.into_iter().next())
    }

    pub fn find_thread(&self, thread_id: &str) -> Result<Option<ThreadSummary>> {
        let mut thread = self
            .cached_threads(true)?
            .into_iter()
            .find(|thread| thread.id == thread_id);
        if let Some(thread) = &mut thread {
            populate_git_branches(std::slice::from_mut(thread), &self.git_branch_cache);
        }
        Ok(thread)
    }

    pub fn composer_settings(&self, thread_id: &str) -> Result<(Option<String>, Option<String>)> {
        let Some(thread) = self.find_thread(thread_id)? else {
            return Ok((None, None));
        };
        let file = File::open(&thread.rollout_path)
            .with_context(|| format!("failed to open {}", thread.rollout_path.display()))?;
        let mut model = None;
        let mut effort = None;
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else { continue };
            let Ok(record) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(payload) = record.get("payload") else {
                continue;
            };
            if let Some(value) = payload.get("model").and_then(Value::as_str) {
                model = Some(value.to_owned());
            }
            if let Some(value) = payload.get("effort").and_then(Value::as_str) {
                effort = Some(value.to_owned());
            }
            if let Some(settings) = payload.get("thread_settings") {
                if let Some(value) = settings.get("model").and_then(Value::as_str) {
                    model = Some(value.to_owned());
                }
                if let Some(value) = settings.get("reasoning_effort").and_then(Value::as_str) {
                    effort = Some(value.to_owned());
                }
            }
        }
        Ok((model, effort))
    }

    pub fn list_projects(&self, include_archived: bool) -> Result<Vec<ProjectSummary>> {
        let threads = self.cached_threads(include_archived)?;
        let mut projects = HashMap::<PathBuf, ProjectSummary>::new();
        for thread in threads {
            let path = project_root_for_cwd(&thread.cwd);
            let entry = projects
                .entry(path.clone())
                .or_insert_with(|| ProjectSummary {
                    name: project_name(&path),
                    path,
                    thread_count: 0,
                    archived_count: 0,
                    updated_at_ms: 0,
                });
            entry.thread_count += 1;
            entry.archived_count += usize::from(thread.archived);
            entry.updated_at_ms = entry.updated_at_ms.max(thread.updated_at_ms);
        }
        let mut projects = projects.into_values().collect::<Vec<_>>();
        projects.sort_by(|left, right| {
            right
                .updated_at_ms
                .cmp(&left.updated_at_ms)
                .then_with(|| left.path.cmp(&right.path))
        });
        Ok(projects)
    }

    pub fn list_project_threads(
        &self,
        project_path: &Path,
        include_archived: bool,
        offset: usize,
        limit: usize,
        pinned_thread_ids: &[String],
    ) -> Result<(Vec<ProjectThreadSummary>, usize)> {
        let mut threads = self
            .cached_threads(include_archived)?
            .into_iter()
            .filter(|thread| project_root_for_cwd(&thread.cwd) == project_path)
            .collect::<Vec<_>>();
        let pinned_ranks = pinned_thread_ids
            .iter()
            .enumerate()
            .map(|(rank, id)| (id.as_str(), rank))
            .collect::<HashMap<_, _>>();
        threads.sort_by_key(|thread| {
            pinned_ranks
                .get(thread.id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        let available = threads.len();
        if offset >= available {
            return Ok((Vec::new(), available));
        }
        let end = available.min(offset.saturating_add(limit));
        threads = threads[offset..end].to_vec();
        populate_git_branches(&mut threads, &self.git_branch_cache);
        Ok((
            threads
                .into_iter()
                .map(|thread| {
                    let pinned = pinned_ranks.contains_key(thread.id.as_str());
                    ProjectThreadSummary {
                        pinned,
                        ..ProjectThreadSummary::from(thread)
                    }
                })
                .collect(),
            available,
        ))
    }

    fn cached_threads(&self, include_archived: bool) -> Result<Vec<ThreadSummary>> {
        if let Ok(cache) = self.summary_cache.lock() {
            if let Some(cache) = cache.as_ref() {
                if cache.refreshed_at.elapsed() <= SUMMARY_CACHE_TTL {
                    return Ok(filter_archived(cache.threads.clone(), include_archived));
                }
            }
        }

        let threads = self.scan_threads()?;
        if let Ok(mut cache) = self.summary_cache.lock() {
            *cache = Some(SummaryCache {
                refreshed_at: Instant::now(),
                threads: threads.clone(),
            });
        }
        Ok(filter_archived(threads, include_archived))
    }

    fn scan_threads(&self) -> Result<Vec<ThreadSummary>> {
        let titles = self.read_title_index()?;
        let mut files = Vec::new();
        collect_rollout_files(&self.codex_home.join("sessions"), false, &mut files)?;
        collect_rollout_files(&self.codex_home.join("archived_sessions"), true, &mut files)?;

        let mut by_id = HashMap::<(String, bool), ThreadSummary>::new();
        for (path, archived) in files {
            let Some(summary) = read_rollout_summary(&path, archived, &titles)? else {
                continue;
            };

            let key = (summary.id.clone(), summary.archived);
            match by_id.get(&key) {
                Some(existing) if existing.updated_at_ms >= summary.updated_at_ms => {}
                _ => {
                    by_id.insert(key, summary);
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
        let messages = self
            .messages_for_path(&summary.rollout_path)?
            .as_ref()
            .clone();
        Ok(Some(ThreadSnapshot {
            thread: summary,
            messages,
        }))
    }

    pub fn read_message_page(
        &self,
        thread_id: &str,
        before: Option<usize>,
        limit: usize,
    ) -> Result<Option<(ThreadSummary, MessagePage)>> {
        let Some(summary) = self.find_thread(thread_id)? else {
            return Ok(None);
        };
        let messages = self.messages_for_path(&summary.rollout_path)?;
        let total = messages.len();
        let end = before.unwrap_or(total).min(total);
        let start = end.saturating_sub(limit);
        Ok(Some((
            summary,
            MessagePage {
                messages: messages[start..end].to_vec(),
                start,
                end,
                total,
                has_more: start > 0,
            },
        )))
    }

    pub fn read_message_content(
        &self,
        thread_id: &str,
        message_index: usize,
        content_index: usize,
        content_end: Option<usize>,
    ) -> Result<Option<Value>> {
        let Some(summary) = self.find_thread(thread_id)? else {
            return Ok(None);
        };
        let messages = self.messages_for_path(&summary.rollout_path)?;
        let Some(message) = messages.get(message_index) else {
            return Ok(None);
        };
        if let Some(end) = content_end {
            if content_index >= end || end > message.content.len() {
                return Ok(None);
            }
            return Ok(Some(Value::Array(
                message.content[content_index..end].to_vec(),
            )));
        }
        Ok(message.content.get(content_index).cloned())
    }

    pub fn read_message(
        &self,
        thread_id: &str,
        message_index: usize,
    ) -> Result<Option<ThreadMessage>> {
        let Some(summary) = self.find_thread(thread_id)? else {
            return Ok(None);
        };
        let messages = self.messages_for_path(&summary.rollout_path)?;
        Ok(messages.get(message_index).cloned())
    }

    fn messages_for_path(&self, path: &Path) -> Result<Arc<Vec<ThreadMessage>>> {
        let metadata =
            fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
        let modified = metadata.modified().ok();
        let file_len = metadata.len();
        if let Ok(mut cache) = self.message_cache.lock() {
            if let Some(entry) = cache.get_mut(path) {
                if entry.modified == modified && entry.file_len == file_len {
                    entry.used_at = Instant::now();
                    return Ok(entry.messages.clone());
                }
            }
        }

        let messages = Arc::new(read_rollout_messages(path)?);
        if let Ok(mut cache) = self.message_cache.lock() {
            if cache.len() >= MESSAGE_CACHE_ENTRIES && !cache.contains_key(path) {
                let oldest = cache
                    .iter()
                    .min_by_key(|(_, entry)| entry.used_at)
                    .map(|(path, _)| path.clone());
                if let Some(oldest) = oldest {
                    cache.remove(&oldest);
                }
            }
            cache.insert(
                path.to_path_buf(),
                CachedMessages {
                    modified,
                    file_len,
                    used_at: Instant::now(),
                    messages: messages.clone(),
                },
            );
        }
        Ok(messages)
    }

    pub fn active_turn_id(&self, thread_id: &str) -> Result<Option<String>> {
        Ok(self
            .thread_activity(thread_id)?
            .and_then(|activity| activity.active_turn_id))
    }

    pub fn thread_activity(&self, thread_id: &str) -> Result<Option<ThreadActivity>> {
        let Some(summary) = self.find_thread(thread_id)? else {
            return Ok(None);
        };
        read_thread_activity(&summary.rollout_path, &self.activity_cache).map(Some)
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

impl From<ThreadSummary> for ProjectThreadSummary {
    fn from(thread: ThreadSummary) -> Self {
        Self {
            id: thread.id,
            title: thread.title,
            cwd: thread.cwd,
            git_branch: thread.git_branch,
            created_at: thread.created_at,
            updated_at_ms: thread.updated_at_ms,
            source: thread.source,
            archived: thread.archived,
            pinned: false,
        }
    }
}

fn filter_archived(mut threads: Vec<ThreadSummary>, include_archived: bool) -> Vec<ThreadSummary> {
    if !include_archived {
        threads.retain(|thread| !thread.archived);
    }
    let mut by_id = HashMap::<String, ThreadSummary>::new();
    for thread in threads {
        match by_id.get(&thread.id) {
            Some(existing) if !existing.archived || thread.archived => {}
            _ => {
                by_id.insert(thread.id.clone(), thread);
            }
        }
    }
    let mut threads = by_id.into_values().collect::<Vec<_>>();
    threads.sort_by(|left, right| {
        right
            .updated_at_ms
            .cmp(&left.updated_at_ms)
            .then_with(|| right.id.cmp(&left.id))
    });
    threads
}

fn project_root_for_cwd(cwd: &Path) -> PathBuf {
    for ancestor in cwd.ancestors() {
        if ancestor.join(".git").exists() {
            return ancestor.to_path_buf();
        }
    }
    cwd.to_path_buf()
}

fn project_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Unknown project")
        .to_owned()
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

    for (line_number, line) in BufReader::new(file).lines().enumerate() {
        if line_number >= 64 {
            break;
        }
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
            if payload.get("source").is_some_and(is_internal_source) {
                return Ok(None);
            }
            id = string_field(payload, "id").or_else(|| string_field(payload, "session_id"));
            cwd = string_field(payload, "cwd").map(PathBuf::from);
            created_at =
                string_field(payload, "timestamp").or_else(|| string_field(&record, "timestamp"));
            source = string_field(payload, "source");

            if id.as_ref().is_some_and(|id| {
                titles
                    .get(id)
                    .is_some_and(|title| real_user_text(title).is_some())
            }) {
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

    let indexed_title = titles
        .get(&id)
        .and_then(|title| real_user_text(title))
        .map(str::to_owned);
    Ok(Some(ThreadSummary {
        title: indexed_title.or(fallback_title),
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

fn populate_git_branches(threads: &mut [ThreadSummary], cache: &GitBranchCache) {
    let now = Instant::now();
    let mut by_cwd = cache
        .lock()
        .map(|mut cache| {
            cache
                .retain(|_, (cached_at, _)| now.duration_since(*cached_at) <= GIT_BRANCH_CACHE_TTL);
            cache
                .iter()
                .map(|(cwd, (_, branch))| (cwd.clone(), branch.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();

    let missing = threads
        .iter()
        .map(|thread| thread.cwd.clone())
        .filter(|cwd| !by_cwd.contains_key(cwd))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let mut resolved = Vec::new();
    for chunk in missing.chunks(GIT_BRANCH_PARALLELISM) {
        let results = std::thread::scope(|scope| {
            chunk
                .iter()
                .map(|cwd| {
                    let cwd = cwd.clone();
                    scope.spawn(move || {
                        let branch = git_branch_for_cwd(&cwd);
                        (cwd, branch)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .collect::<Vec<_>>()
        });
        by_cwd.extend(results.iter().cloned());
        resolved.extend(results);
    }

    if let Ok(mut cache) = cache.lock() {
        for (cwd, branch) in resolved {
            cache.insert(cwd, (now, branch));
        }
    }

    for thread in threads {
        thread.git_branch = by_cwd.get(&thread.cwd).cloned().flatten();
    }
}

fn git_branch_for_cwd(cwd: &Path) -> Option<String> {
    if !cwd.is_dir() {
        return None;
    }
    let mut child = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .spawn()
        .ok()?;

    // Git on a cloud-synced or offloaded repo (e.g. ~/Documents on iCloud Drive)
    // can block for minutes waiting for file materialization. Bound the wait so a
    // single pathological checkout cannot stall `ls`/`show`/`current` forever.
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    let mut stdout = String::new();
    child.stdout.take()?.read_to_string(&mut stdout).ok()?;
    if stdout.len() > 4 * 1024 {
        return None;
    }
    let branch = stdout.trim();
    (!branch.is_empty()).then(|| branch.to_owned())
}

fn read_rollout_messages(path: &Path) -> Result<Vec<ThreadMessage>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut messages = Vec::<ThreadMessage>::new();
    let mut seen_ids = HashSet::new();
    let mut tool_locations = HashMap::<String, (usize, usize)>::new();

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
        let payload_type = payload.get("type").and_then(Value::as_str);
        if matches!(payload_type, Some("custom_tool_call" | "function_call")) {
            let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                continue;
            };
            let message_index = messages
                .last()
                .filter(|message| message.role == "assistant")
                .map(|_| messages.len() - 1)
                .unwrap_or_else(|| {
                    messages.push(ThreadMessage {
                        timestamp: string_field(&record, "timestamp"),
                        id: None,
                        role: "assistant".to_owned(),
                        phase: Some("tool".to_owned()),
                        content: Vec::new(),
                        tools: Vec::new(),
                    });
                    messages.len() - 1
                });
            let name = match (
                payload.get("namespace").and_then(Value::as_str),
                payload.get("name").and_then(Value::as_str),
            ) {
                (Some(namespace), Some(name)) => format!("{namespace}.{name}"),
                (_, Some(name)) => name.to_owned(),
                _ => "tool".to_owned(),
            };
            let input = payload
                .get("input")
                .or_else(|| payload.get("arguments"))
                .cloned()
                .unwrap_or(Value::Null);
            let tool_index = messages[message_index].tools.len();
            messages[message_index].tools.push(ThreadToolCall {
                call_id: call_id.to_owned(),
                name,
                status: string_field(payload, "status").unwrap_or_else(|| "running".to_owned()),
                input,
                output: None,
            });
            tool_locations.insert(call_id.to_owned(), (message_index, tool_index));
            continue;
        }
        if matches!(
            payload_type,
            Some("custom_tool_call_output" | "function_call_output")
        ) {
            let Some(call_id) = payload.get("call_id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(&(message_index, tool_index)) = tool_locations.get(call_id) {
                let tool = &mut messages[message_index].tools[tool_index];
                tool.output = payload.get("output").cloned();
                if tool.status == "running" {
                    tool.status = "completed".to_owned();
                }
            }
            continue;
        }
        if payload_type != Some("message") {
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
            tools: Vec::new(),
        });
    }

    Ok(messages)
}

fn read_thread_activity(
    path: &Path,
    cache: &Arc<Mutex<HashMap<PathBuf, CachedActivity>>>,
) -> Result<ThreadActivity> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    let file_len = metadata.len();
    let updated_at_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(0);
    let cached = cache.lock().ok().and_then(|cache| cache.get(path).cloned());
    if let Some(cached) = cached
        .as_ref()
        .filter(|cached| cached.processed_len == file_len)
    {
        return Ok(ThreadActivity {
            file_len,
            updated_at_ms,
            active_turn_id: cached.active_turn_id.clone(),
            phase: activity_phase(&cached.active_turn_id, &cached.active_tools),
            active_tool: cached.active_tools.last().map(|(_, name)| name.clone()),
        });
    }

    let (start, mut active_turn_id, mut active_tools) = cached
        .filter(|cached| cached.processed_len <= file_len)
        .map(|cached| {
            (
                cached.processed_len,
                cached.active_turn_id,
                cached.active_tools,
            )
        })
        .unwrap_or((0, None, Vec::new()));
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.seek(SeekFrom::Start(start))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut processed_len = start;
    loop {
        let mut line = String::new();
        let bytes = match reader.read_line(&mut line) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::InvalidData => break,
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        if bytes == 0 || !line.ends_with('\n') {
            break;
        }
        processed_len = processed_len.saturating_add(bytes as u64);
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        update_activity(&record, &mut active_turn_id, &mut active_tools);
    }
    if let Ok(mut cache) = cache.lock() {
        cache.insert(
            path.to_path_buf(),
            CachedActivity {
                processed_len,
                active_turn_id: active_turn_id.clone(),
                active_tools: active_tools.clone(),
            },
        );
    }
    Ok(ThreadActivity {
        file_len,
        updated_at_ms,
        phase: activity_phase(&active_turn_id, &active_tools),
        active_tool: active_tools.last().map(|(_, name)| name.clone()),
        active_turn_id,
    })
}

fn activity_phase(
    active_turn_id: &Option<String>,
    active_tools: &[(String, String)],
) -> Option<String> {
    active_turn_id.as_ref()?;
    Some(if active_tools.is_empty() {
        "model".to_owned()
    } else {
        "tool".to_owned()
    })
}

fn update_activity(
    record: &Value,
    active_turn_id: &mut Option<String>,
    active_tools: &mut Vec<(String, String)>,
) {
    if record.get("type").and_then(Value::as_str) == Some("response_item") {
        let payload = &record["payload"];
        let payload_type = payload.get("type").and_then(Value::as_str);
        let call_id = payload
            .get("call_id")
            .or_else(|| payload.get("id"))
            .and_then(Value::as_str);
        match payload_type {
            Some("custom_tool_call" | "function_call") => {
                if let Some(call_id) = call_id {
                    active_tools.retain(|(existing, _)| existing != call_id);
                    active_tools.push((
                        call_id.to_owned(),
                        payload
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_owned(),
                    ));
                }
            }
            Some("custom_tool_call_output" | "function_call_output") => {
                if let Some(call_id) = call_id {
                    active_tools.retain(|(existing, _)| existing != call_id);
                }
            }
            _ => {}
        }
        return;
    }
    if record.get("type").and_then(Value::as_str) != Some("event_msg") {
        return;
    }
    let payload = &record["payload"];
    let event_type = payload.get("type").and_then(Value::as_str);
    let turn_id = payload.get("turn_id").and_then(Value::as_str);
    match event_type {
        Some("task_started") => {
            *active_turn_id = turn_id.map(str::to_owned);
            active_tools.clear();
        }
        Some("task_complete" | "turn_aborted")
            if turn_id.is_none() || turn_id == active_turn_id.as_deref() =>
        {
            *active_turn_id = None;
            active_tools.clear();
        }
        _ => {}
    }
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
        .filter_map(real_user_text)
        .collect::<Vec<_>>()
        .join(" ");
    let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        None
    } else {
        Some(title.chars().take(120).collect())
    }
}

fn real_user_text(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with("# Files mentioned by the user:") {
        return text
            .split_once("## My request:")
            .map(|(_, request)| request.trim())
            .filter(|request| !request.is_empty());
    }
    if trimmed.starts_with(
        "The following is the Codex agent history whose request action you are assessing.",
    ) {
        return text
            .split_once("\n[1] user:")
            .map(|(_, transcript)| transcript)
            .and_then(|transcript| transcript.split_once("\n\n[2]").map(|(request, _)| request))
            .map(str::trim)
            .filter(|request| !request.is_empty());
    }
    if let Some(request) = trimmed.strip_prefix("[1] user:") {
        return (!request.trim().is_empty()).then_some(request.trim());
    }
    let injected = trimmed.starts_with(">>> TRANSCRIPT DELTA START")
        || trimmed.starts_with(">>> TRANSCRIPT START")
        || trimmed.starts_with("<recommended_plugins>")
        || trimmed.starts_with("# AGENTS.md instructions")
        || trimmed.starts_with("<environment_context>")
        || trimmed.starts_with("<permissions instructions>")
        || trimmed.starts_with("<skills_instructions>")
        || trimmed.starts_with("<collaboration_mode>")
        || trimmed.starts_with("<multi_agent_mode>")
        || trimmed.starts_with("<image name=")
        || trimmed == "</image>";
    (!injected && !trimmed.is_empty()).then_some(trimmed)
}

fn string_field(value: &Value, name: &str) -> Option<String> {
    value.get(name).and_then(Value::as_str).map(str::to_owned)
}

fn is_internal_source(source: &Value) -> bool {
    source
        .as_object()
        .is_some_and(|source| source.contains_key("subagent"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
                "codex-bridge-session-test-{}-{nonce}-{}",
                std::process::id(),
                FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
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
    fn reads_latest_composer_settings_from_rollout_records() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-settings.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-settings","cwd":"/tmp/project","model":"gpt-old","effort":"low"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"event_msg","payload":{"thread_settings":{"model":"gpt-new","reasoning_effort":"high"}}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"turn_context","payload":{"model":"gpt-new","effort":"medium"}}"#,
                "{incomplete",
            ],
        );

        let store = SessionStore::new(fixture.path.clone());
        assert_eq!(
            store.composer_settings("thread-settings").unwrap(),
            (Some("gpt-new".to_owned()), Some("medium".to_owned()))
        );
    }

    #[test]
    fn groups_nested_session_directories_under_the_repository_root() {
        let fixture = Fixture::new();
        let workspace = fixture.path.join("workspace");
        init_git_repo(&workspace, "main");
        let first_cwd = workspace.join("crates/one");
        let second_cwd = workspace.join("examples/two");
        fs::create_dir_all(&first_cwd).unwrap();
        fs::create_dir_all(&second_cwd).unwrap();
        let first = format!(
            r#"{{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{{"id":"thread-one","cwd":{},"source":"fixture"}}}}"#,
            serde_json::to_string(&first_cwd).unwrap()
        );
        let second = format!(
            r#"{{"timestamp":"2026-08-30T02:00:00Z","type":"session_meta","payload":{{"id":"thread-two","cwd":{},"source":"fixture"}}}}"#,
            serde_json::to_string(&second_cwd).unwrap()
        );
        fixture.write_rollout("rollout-one.jsonl", &[&first]);
        fixture.write_rollout("rollout-two.jsonl", &[&second]);

        let store = SessionStore::new(fixture.path.clone());
        let projects = store.list_projects(false).unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].path, workspace);
        assert_eq!(projects[0].thread_count, 2);

        let (threads, available) = store
            .list_project_threads(&projects[0].path, false, 0, 1, &["thread-one".to_owned()])
            .unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(available, 2);
        assert_eq!(threads[0].id, "thread-one");
        assert!(threads[0].pinned);
        assert_ne!(threads[0].cwd, projects[0].path);
    }

    #[test]
    fn message_pages_only_return_the_requested_slice() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-page.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-page","cwd":"/tmp/project"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"response_item","payload":{"type":"message","id":"m1","role":"user","content":[{"text":"one"}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"response_item","payload":{"type":"message","id":"m2","role":"assistant","content":[{"text":"two"}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:03Z","type":"response_item","payload":{"type":"message","id":"m3","role":"user","content":[{"text":"three"}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:04Z","type":"response_item","payload":{"type":"message","id":"m4","role":"assistant","content":[{"text":"four"}]}}"#,
            ],
        );
        fs::write(
            fixture.path.join("session_index.jsonl"),
            r#"{"id":"thread-title","thread_name":"<environment_context>automatic</environment_context>","updated_at":"2026-08-30T01:00:03Z"}
"#,
        )
        .unwrap();
        let store = SessionStore::new(fixture.path.clone());

        let (_, latest) = store
            .read_message_page("thread-page", None, 2)
            .unwrap()
            .unwrap();
        assert_eq!((latest.start, latest.end, latest.total), (2, 4, 4));
        assert!(latest.has_more);
        assert_eq!(latest.messages[0].id.as_deref(), Some("m3"));

        let (_, older) = store
            .read_message_page("thread-page", Some(latest.start), 2)
            .unwrap()
            .unwrap();
        assert_eq!((older.start, older.end), (0, 2));
        assert!(!older.has_more);
        assert_eq!(older.messages[1].id.as_deref(), Some("m2"));
    }

    #[test]
    fn tool_calls_and_outputs_attach_to_the_preceding_assistant_message() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-tools.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-tools","cwd":"/tmp/project","source":"vscode"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"response_item","payload":{"type":"message","id":"a1","role":"assistant","content":[{"type":"output_text","text":"I will inspect it."}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call-1","name":"exec","status":"completed","input":"{\"cmd\":\"git status --short\"}"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:03Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-1","output":"clean"}}"#,
            ],
        );
        let store = SessionStore::new(fixture.path.clone());
        let snapshot = store.read_thread("thread-tools").unwrap().unwrap();
        assert_eq!(snapshot.messages.len(), 1);
        assert_eq!(snapshot.messages[0].tools.len(), 1);
        assert_eq!(snapshot.messages[0].tools[0].name, "exec");
        assert_eq!(
            snapshot.messages[0].tools[0]
                .output
                .as_ref()
                .and_then(Value::as_str),
            Some("clean")
        );
        assert_eq!(
            store
                .read_message("thread-tools", 0)
                .unwrap()
                .unwrap()
                .tools[0]
                .call_id,
            "call-1"
        );
    }

    #[test]
    fn fallback_title_skips_injected_user_context() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-title.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-title","cwd":"/tmp/project"}}"#,
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":">>> TRANSCRIPT DELTA START\nautomatic\n>>> TRANSCRIPT DELTA END"},{"type":"input_text","text":"<environment_context>automatic</environment_context>"}]}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real user request"}]}}"#,
            ],
        );
        let store = SessionStore::new(fixture.path.clone());
        let threads = store.list_threads(false).unwrap();
        assert_eq!(threads[0].title.as_deref(), Some("real user request"));
        assert_eq!(
            real_user_text(
                "The following is the Codex agent history whose request action you are assessing.\n\n>>> TRANSCRIPT START\n\n[1] user: original request\n\n[2] assistant: working"
            ),
            Some("original request")
        );
    }

    #[test]
    fn internal_subagent_rollouts_are_not_user_threads() {
        let fixture = Fixture::new();
        fixture.write_rollout(
            "rollout-user.jsonl",
            &[r#"{"timestamp":"2026-08-30T01:00:00Z","type":"session_meta","payload":{"id":"thread-user","cwd":"/tmp/project","source":"vscode"}}"#],
        );
        fixture.write_rollout(
            "rollout-guardian.jsonl",
            &[
                r#"{"timestamp":"2026-08-30T01:00:01Z","type":"session_meta","payload":{"id":"thread-guardian","cwd":"/tmp/project","source":{"subagent":{"other":"guardian"}}}}"#,
                r#"{"timestamp":"2026-08-30T01:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"The following is the Codex agent history whose request action you are assessing."}]}}"#,
            ],
        );
        fixture.write_rollout(
            "rollout-worker.jsonl",
            &[r#"{"timestamp":"2026-08-30T01:00:03Z","type":"session_meta","payload":{"id":"thread-worker","cwd":"/tmp/project","source":{"subagent":{"thread_spawn":{"parent_thread_id":"thread-user","depth":1}}}}}"#],
        );

        let store = SessionStore::new(fixture.path.clone());
        let threads = store.list_threads(false).unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].id, "thread-user");
        assert_eq!(store.list_projects(false).unwrap()[0].thread_count, 1);
        assert!(store.find_thread("thread-guardian").unwrap().is_none());
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
        assert_eq!(
            store
                .thread_activity("thread-active")
                .unwrap()
                .unwrap()
                .phase,
            Some("model".to_owned())
        );

        let path = fixture
            .path
            .join("sessions/2026/08/30/rollout-active.jsonl");
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(
            br#"{"timestamp":"2026-08-30T01:00:04Z","type":"response_item","payload":{"type":"custom_tool_call","call_id":"call-1","name":"exec","status":"completed","input":"{}"}}
"#,
        )
        .unwrap();
        let activity = store.thread_activity("thread-active").unwrap().unwrap();
        assert_eq!(activity.phase, Some("tool".to_owned()));
        assert_eq!(activity.active_tool, Some("exec".to_owned()));

        file.write_all(
            br#"{"timestamp":"2026-08-30T01:00:05Z","type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"call-1","output":"done"}}
"#,
        )
        .unwrap();
        let activity = store.thread_activity("thread-active").unwrap().unwrap();
        assert_eq!(activity.phase, Some("model".to_owned()));
        assert_eq!(activity.active_tool, None);

        file.write_all(
            br#"{"timestamp":"2026-08-30T01:00:06Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-active"}}
"#,
        )
        .unwrap();
        assert_eq!(store.active_turn_id("thread-active").unwrap(), None);
    }
}
