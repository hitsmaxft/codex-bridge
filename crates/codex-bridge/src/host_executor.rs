use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub const HOST_EXEC_POLICY_ENV: &str = "CODEX_BRIDGE_HOST_EXEC_POLICY";
pub const DEFAULT_HOST_EXEC_TIMEOUT_SECONDS: u64 = 300;
pub const MAX_HOST_EXEC_TIMEOUT_SECONDS: u64 = 3_600;
pub const HOST_EXEC_OUTPUT_LIMIT_BYTES: usize = 32 * 1024;

const MAX_HOST_EXEC_ARGS: usize = 128;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SAFE_ENVIRONMENT_KEYS: &[&str] = &[
    "HOME", "LANG", "LC_ALL", "LOGNAME", "PATH", "TMPDIR", "USER",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostExecPolicyConfig {
    /// Executable names resolved through absolute entries in the daemon PATH.
    #[serde(default)]
    pub executables: Vec<String>,
    /// Executable paths relative to the selected thread workspace. One `*` is supported.
    #[serde(default)]
    pub workspace_paths: Vec<String>,
    /// Git subcommands allowed through the hardened git invocation.
    #[serde(default)]
    pub git_subcommands: Vec<String>,
    #[serde(default = "default_timeout_seconds")]
    pub default_timeout_seconds: u64,
    #[serde(default = "max_timeout_seconds")]
    pub max_timeout_seconds: u64,
}

impl Default for HostExecPolicyConfig {
    fn default() -> Self {
        Self {
            executables: vec!["wlink".to_owned()],
            workspace_paths: vec!["cases/run-ch585-*.sh".to_owned()],
            git_subcommands: [
                "describe",
                "diff",
                "log",
                "ls-files",
                "rev-parse",
                "show",
                "status",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            default_timeout_seconds: DEFAULT_HOST_EXEC_TIMEOUT_SECONDS,
            max_timeout_seconds: MAX_HOST_EXEC_TIMEOUT_SECONDS,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostExecPolicySummary {
    pub source: String,
    pub executables: Vec<String>,
    pub workspace_paths: Vec<String>,
    pub git_subcommands: Vec<String>,
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
    pub output_limit_bytes_per_stream: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct HostExecResult {
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub executable: PathBuf,
    pub policy_rule: String,
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecFailure {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct HostExecutor {
    policy: HostExecPolicyConfig,
    source: String,
}

#[derive(Debug)]
struct AuthorizedCommand {
    executable: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    policy_rule: String,
}

#[derive(Debug)]
struct CapturedStream {
    text: Option<String>,
    truncated: bool,
}

impl HostExecutor {
    pub fn load(policy_path: Option<&Path>) -> Result<Self, HostExecFailure> {
        match policy_path {
            Some(path) => {
                let bytes = fs::read(path).map_err(|error| HostExecFailure {
                    code: "host_exec_policy_error",
                    message: format!(
                        "failed to read host-exec policy {}: {error}",
                        path.display()
                    ),
                })?;
                let policy =
                    serde_json::from_slice::<HostExecPolicyConfig>(&bytes).map_err(|error| {
                        HostExecFailure {
                            code: "host_exec_policy_error",
                            message: format!(
                                "failed to parse host-exec policy {}: {error}",
                                path.display()
                            ),
                        }
                    })?;
                Self::from_policy(policy, path.display().to_string())
            }
            None => Self::from_policy(HostExecPolicyConfig::default(), "built_in".to_owned()),
        }
    }

    pub fn from_policy(
        mut policy: HostExecPolicyConfig,
        source: String,
    ) -> Result<Self, HostExecFailure> {
        validate_policy(&policy)?;
        policy.executables.sort();
        policy.executables.dedup();
        policy.workspace_paths.sort();
        policy.workspace_paths.dedup();
        policy.git_subcommands.sort();
        policy.git_subcommands.dedup();
        Ok(Self { policy, source })
    }

    pub fn summary(&self) -> HostExecPolicySummary {
        HostExecPolicySummary {
            source: self.source.clone(),
            executables: self.policy.executables.clone(),
            workspace_paths: self.policy.workspace_paths.clone(),
            git_subcommands: self.policy.git_subcommands.clone(),
            default_timeout_seconds: self.policy.default_timeout_seconds,
            max_timeout_seconds: self.policy.max_timeout_seconds,
            output_limit_bytes_per_stream: HOST_EXEC_OUTPUT_LIMIT_BYTES,
        }
    }

    pub fn execute(
        &self,
        cwd: &Path,
        argv: &[String],
        requested_timeout_seconds: Option<u64>,
    ) -> Result<HostExecResult, HostExecFailure> {
        validate_request(argv)?;
        let timeout_seconds =
            requested_timeout_seconds.unwrap_or(self.policy.default_timeout_seconds);
        if timeout_seconds == 0 || timeout_seconds > self.policy.max_timeout_seconds {
            return Err(HostExecFailure {
                code: "host_exec_invalid_request",
                message: format!(
                    "host-exec timeout must be between 1 and {} seconds",
                    self.policy.max_timeout_seconds
                ),
            });
        }
        self.execute_with_timeout(cwd, argv, Duration::from_secs(timeout_seconds))
    }

    fn execute_with_timeout(
        &self,
        cwd: &Path,
        argv: &[String],
        timeout: Duration,
    ) -> Result<HostExecResult, HostExecFailure> {
        validate_request(argv)?;
        let authorized = self.authorize(cwd, argv)?;
        let mut command = Command::new(&authorized.executable);
        command
            .args(&authorized.args)
            .current_dir(&authorized.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env_clear()
            .process_group(0);
        for key in SAFE_ENVIRONMENT_KEYS {
            if let Some(value) = env::var_os(key) {
                command.env(key, value);
            }
        }
        command
            .env("GIT_PAGER", "cat")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0");

        let started = Instant::now();
        let mut child = command.spawn().map_err(|error| HostExecFailure {
            code: "host_exec_spawn_failed",
            message: format!(
                "failed to start allowed executable {}: {error}",
                authorized.executable.display()
            ),
        })?;
        let stdout = child.stdout.take().expect("piped child stdout");
        let stderr = child.stderr.take().expect("piped child stderr");
        let stdout_reader = thread::spawn(move || capture_stream(stdout));
        let stderr_reader = thread::spawn(move || capture_stream(stderr));

        let (status, timed_out) = wait_for_child(&mut child, timeout)?;
        let stdout = join_capture(stdout_reader, "stdout")?;
        let stderr = join_capture(stderr_reader, "stderr")?;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        Ok(HostExecResult {
            argv: argv.to_vec(),
            cwd: authorized.cwd,
            executable: authorized.executable,
            policy_rule: authorized.policy_rule,
            exit_code: status.code().unwrap_or(-1),
            timed_out,
            duration_ms,
            stdout: stdout.text,
            stderr: stderr.text,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
        })
    }

    fn authorize(&self, cwd: &Path, argv: &[String]) -> Result<AuthorizedCommand, HostExecFailure> {
        let cwd = fs::canonicalize(cwd).map_err(|error| HostExecFailure {
            code: "host_exec_workspace_error",
            message: format!("cannot resolve thread workspace {}: {error}", cwd.display()),
        })?;
        let command = &argv[0];

        if !command.contains('/') && self.policy.executables.iter().any(|item| item == command) {
            let executable = resolve_path_executable(command)?;
            return Ok(AuthorizedCommand {
                executable,
                args: argv[1..].iter().map(OsString::from).collect(),
                cwd,
                policy_rule: format!("executable:{command}"),
            });
        }

        if command == "git" {
            return self.authorize_git(cwd, argv);
        }

        let relative = command.strip_prefix("./").unwrap_or(command);
        if is_safe_relative_path(Path::new(relative)) {
            if let Some(pattern) = self
                .policy
                .workspace_paths
                .iter()
                .find(|pattern| wildcard_match(pattern, relative))
            {
                let candidate = cwd.join(relative);
                let executable = fs::canonicalize(&candidate).map_err(|error| HostExecFailure {
                    code: "host_exec_unavailable",
                    message: format!(
                        "cannot resolve allowed workspace command {relative}: {error}"
                    ),
                })?;
                if !executable.starts_with(&cwd) {
                    return Err(not_allowed(
                        command,
                        "workspace command resolves outside its thread workspace",
                    ));
                }
                ensure_executable_file(&executable)?;
                return Ok(AuthorizedCommand {
                    executable,
                    args: argv[1..].iter().map(OsString::from).collect(),
                    cwd,
                    policy_rule: format!("workspace_path:{pattern}"),
                });
            }
        }

        Err(not_allowed(
            command,
            "command does not match the configured host-exec policy",
        ))
    }

    fn authorize_git(
        &self,
        cwd: PathBuf,
        argv: &[String],
    ) -> Result<AuthorizedCommand, HostExecFailure> {
        let Some(subcommand) = argv.get(1) else {
            return Err(not_allowed("git", "git requires an allowed subcommand"));
        };
        if subcommand.starts_with('-')
            || !self
                .policy
                .git_subcommands
                .iter()
                .any(|allowed| allowed == subcommand)
        {
            return Err(not_allowed(
                "git",
                &format!("git subcommand {subcommand:?} is not allowed"),
            ));
        }
        if argv[2..]
            .iter()
            .any(|argument| dangerous_git_argument(argument))
        {
            return Err(not_allowed(
                "git",
                "git arguments may not enable external helpers, signature programs, or output files",
            ));
        }

        let executable = resolve_path_executable("git")?;
        let mut args = vec![
            OsString::from("--no-pager"),
            OsString::from("-c"),
            OsString::from("core.fsmonitor=false"),
            OsString::from("-c"),
            OsString::from("diff.external="),
            OsString::from(subcommand),
        ];
        if matches!(subcommand.as_str(), "diff" | "log" | "show") {
            args.extend([
                OsString::from("--no-ext-diff"),
                OsString::from("--no-textconv"),
            ]);
        }
        args.extend(argv[2..].iter().map(OsString::from));
        Ok(AuthorizedCommand {
            executable,
            args,
            cwd,
            policy_rule: format!("git_subcommand:{subcommand}"),
        })
    }
}

fn default_timeout_seconds() -> u64 {
    DEFAULT_HOST_EXEC_TIMEOUT_SECONDS
}

fn max_timeout_seconds() -> u64 {
    MAX_HOST_EXEC_TIMEOUT_SECONDS
}

fn validate_policy(policy: &HostExecPolicyConfig) -> Result<(), HostExecFailure> {
    if policy.default_timeout_seconds == 0
        || policy.default_timeout_seconds > policy.max_timeout_seconds
        || policy.max_timeout_seconds > MAX_HOST_EXEC_TIMEOUT_SECONDS
    {
        return Err(HostExecFailure {
            code: "host_exec_policy_error",
            message: format!(
                "policy timeouts must satisfy 1 <= default <= max <= {MAX_HOST_EXEC_TIMEOUT_SECONDS}"
            ),
        });
    }
    for executable in &policy.executables {
        if executable.is_empty()
            || executable == "git"
            || executable.contains('/')
            || executable == "."
            || executable == ".."
        {
            return Err(HostExecFailure {
                code: "host_exec_policy_error",
                message: format!(
                    "invalid executable rule {executable:?}; use a bare name and configure git through git_subcommands"
                ),
            });
        }
    }
    for pattern in &policy.workspace_paths {
        if !is_safe_relative_path(Path::new(pattern)) || pattern.matches('*').count() > 1 {
            return Err(HostExecFailure {
                code: "host_exec_policy_error",
                message: format!(
                    "invalid workspace path rule {pattern:?}; use a relative path with at most one *"
                ),
            });
        }
    }
    for subcommand in &policy.git_subcommands {
        if subcommand.is_empty()
            || subcommand.starts_with('-')
            || !subcommand
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(HostExecFailure {
                code: "host_exec_policy_error",
                message: format!("invalid git subcommand rule {subcommand:?}"),
            });
        }
    }
    Ok(())
}

fn validate_request(argv: &[String]) -> Result<(), HostExecFailure> {
    if argv.is_empty() || argv[0].is_empty() {
        return Err(HostExecFailure {
            code: "host_exec_invalid_request",
            message: "host-exec requires a non-empty argv".to_owned(),
        });
    }
    if argv.len() > MAX_HOST_EXEC_ARGS {
        return Err(HostExecFailure {
            code: "host_exec_invalid_request",
            message: format!("host-exec accepts at most {MAX_HOST_EXEC_ARGS} arguments"),
        });
    }
    Ok(())
}

fn resolve_path_executable(name: &str) -> Result<PathBuf, HostExecFailure> {
    let path = env::var_os("PATH").ok_or_else(|| HostExecFailure {
        code: "host_exec_unavailable",
        message: format!("PATH is unavailable while resolving allowed executable {name}"),
    })?;
    for directory in env::split_paths(&path).filter(|directory| directory.is_absolute()) {
        let candidate = directory.join(name);
        if ensure_executable_file(&candidate).is_ok() {
            return fs::canonicalize(&candidate).map_err(|error| HostExecFailure {
                code: "host_exec_unavailable",
                message: format!("cannot resolve {}: {error}", candidate.display()),
            });
        }
    }
    Err(HostExecFailure {
        code: "host_exec_unavailable",
        message: format!("allowed executable {name:?} was not found in an absolute PATH entry"),
    })
}

fn ensure_executable_file(path: &Path) -> Result<(), HostExecFailure> {
    let metadata = fs::metadata(path).map_err(|error| HostExecFailure {
        code: "host_exec_unavailable",
        message: format!(
            "cannot inspect allowed executable {}: {error}",
            path.display()
        ),
    })?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(HostExecFailure {
            code: "host_exec_unavailable",
            message: format!(
                "allowed command is not an executable file: {}",
                path.display()
            ),
        });
    }
    Ok(())
}

fn is_safe_relative_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn wildcard_match(pattern: &str, candidate: &str) -> bool {
    let Some((prefix, suffix)) = pattern.split_once('*') else {
        return pattern == candidate;
    };
    let Some(middle) = candidate
        .strip_prefix(prefix)
        .and_then(|candidate| candidate.strip_suffix(suffix))
    else {
        return false;
    };
    !middle.contains('/')
}

fn dangerous_git_argument(argument: &str) -> bool {
    const DANGEROUS: &[&str] = &[
        "--config-env",
        "--exec-path",
        "--ext-diff",
        "--output",
        "--show-signature",
        "--textconv",
    ];
    DANGEROUS.iter().any(|option| {
        argument == *option
            || argument
                .strip_prefix(option)
                .is_some_and(|suffix| suffix.starts_with('='))
    }) || argument.contains("%G")
}

fn not_allowed(command: &str, reason: &str) -> HostExecFailure {
    HostExecFailure {
        code: "host_exec_not_allowed",
        message: format!("host command {command:?} is denied: {reason}"),
    }
}

fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
) -> Result<(ExitStatus, bool), HostExecFailure> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok((status, false)),
            Ok(None) if Instant::now() < deadline => thread::sleep(PROCESS_POLL_INTERVAL),
            Ok(None) => {
                kill_process_group(child);
                let status = child.wait().map_err(|error| HostExecFailure {
                    code: "host_exec_wait_failed",
                    message: format!("failed to wait for timed-out host command: {error}"),
                })?;
                return Ok((status, true));
            }
            Err(error) => {
                kill_process_group(child);
                let _ = child.wait();
                return Err(HostExecFailure {
                    code: "host_exec_wait_failed",
                    message: format!("failed while waiting for host command: {error}"),
                });
            }
        }
    }
}

fn kill_process_group(child: &mut Child) {
    if let Ok(process_group) = i32::try_from(child.id()) {
        // The child was spawned into a new process group whose id equals its pid.
        // killpg therefore targets only this host-exec invocation and descendants.
        unsafe {
            libc::killpg(process_group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

fn capture_stream(mut reader: impl Read) -> io::Result<CapturedStream> {
    let mut output = Vec::with_capacity(HOST_EXEC_OUTPUT_LIMIT_BYTES);
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = HOST_EXEC_OUTPUT_LIMIT_BYTES.saturating_sub(output.len());
        let keep = remaining.min(read);
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(CapturedStream {
        text: bounded_lossy_text(&output),
        truncated,
    })
}

fn bounded_lossy_text(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }
    let mut text = String::from_utf8_lossy(bytes).into_owned();
    if text.len() > HOST_EXEC_OUTPUT_LIMIT_BYTES {
        let mut boundary = HOST_EXEC_OUTPUT_LIMIT_BYTES;
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
    }
    Some(text)
}

fn join_capture(
    reader: thread::JoinHandle<io::Result<CapturedStream>>,
    stream: &str,
) -> Result<CapturedStream, HostExecFailure> {
    reader
        .join()
        .map_err(|_| HostExecFailure {
            code: "host_exec_output_error",
            message: format!("host command {stream} reader panicked"),
        })?
        .map_err(|error| HostExecFailure {
            code: "host_exec_output_error",
            message: format!("failed to read host command {stream}: {error}"),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/host-exec")
    }

    #[test]
    fn default_policy_is_narrow_and_rejects_shells_and_git_global_options() {
        let executor = HostExecutor::load(None).unwrap();
        let root = fixture_root();

        let script = executor
            .authorize(&root, &["cases/run-ch585-fixture.sh".into(), "ok".into()])
            .unwrap();
        assert_eq!(script.policy_rule, "workspace_path:cases/run-ch585-*.sh");

        let shell = executor.authorize(&root, &["sh".into(), "-c".into(), "id".into()]);
        assert_eq!(shell.unwrap_err().code, "host_exec_not_allowed");

        let git = executor.authorize(
            &root,
            &["git".into(), "-c".into(), "alias.x=!id".into(), "x".into()],
        );
        assert_eq!(git.unwrap_err().code, "host_exec_not_allowed");

        let git = executor
            .authorize(&root, &["git".into(), "diff".into(), "--stat".into()])
            .unwrap();
        assert!(git.args.contains(&OsString::from("--no-ext-diff")));
        assert!(git.args.contains(&OsString::from("--no-textconv")));
    }

    #[test]
    fn workspace_rule_cannot_escape_through_a_relative_path() {
        let executor = HostExecutor::load(None).unwrap();
        let result = executor.authorize(
            &fixture_root(),
            &["cases/../cases/run-ch585-fixture.sh".into()],
        );
        assert_eq!(result.unwrap_err().code, "host_exec_not_allowed");
    }

    #[test]
    fn allowed_fixture_returns_exit_output_and_truncation_metadata() {
        let executor = HostExecutor::load(None).unwrap();
        let root = fixture_root();

        let result = executor
            .execute(
                &root,
                &["cases/run-ch585-fixture.sh".into(), "ok".into()],
                None,
            )
            .unwrap();
        assert_eq!(result.exit_code, 7);
        assert!(!result.timed_out);
        assert_eq!(result.stdout.as_deref(), Some("fixture stdout\n"));
        assert_eq!(result.stderr.as_deref(), Some("fixture stderr\n"));

        let result = executor
            .execute(
                &root,
                &["cases/run-ch585-fixture.sh".into(), "flood".into()],
                None,
            )
            .unwrap();
        assert!(result.stdout_truncated);
        assert_eq!(result.stdout.unwrap().len(), HOST_EXEC_OUTPUT_LIMIT_BYTES);
    }

    #[test]
    fn timeout_kills_the_fixture_process_group() {
        let executor = HostExecutor::load(None).unwrap();
        let result = executor
            .execute_with_timeout(
                &fixture_root(),
                &["cases/run-ch585-fixture.sh".into(), "sleep".into()],
                Duration::from_millis(50),
            )
            .unwrap();
        assert!(result.timed_out);
        assert_eq!(result.exit_code, -1);
    }

    #[test]
    fn custom_policy_replaces_the_default_rules() {
        let policy = HostExecPolicyConfig {
            executables: Vec::new(),
            workspace_paths: vec!["tools/probe-*".to_owned()],
            git_subcommands: vec!["status".to_owned()],
            default_timeout_seconds: 10,
            max_timeout_seconds: 20,
        };
        let executor = HostExecutor::from_policy(policy, "test".to_owned()).unwrap();
        let summary = executor.summary();
        assert!(summary.executables.is_empty());
        assert_eq!(summary.workspace_paths, ["tools/probe-*"]);
        assert_eq!(summary.max_timeout_seconds, 20);
    }
}
