#![allow(dead_code)]

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agy::{build_agy_command, run_command_captured_interruptibly_with_timeout};
use crate::config::ProfileRegistry;
use crate::keychain::{activate_profile, apply_profile_home, PROFILE_KEYCHAIN_SETUP_TIMEOUT};
use crate::security::{reject_symlink, validate_profile_dir_under, ProcessLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskFailureKind {
    Timeout,
    EmptyOutput,
    QuotaExhausted,
    PermissionDenied,
    AgentFailed,
    InvalidWorktree,
    InvalidProfile,
    Unauthenticated,
    InvalidPrompt,
    InvalidDuration,
    SetupError,
}

impl TaskFailureKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskFailureKind::Timeout => "timeout",
            TaskFailureKind::EmptyOutput => "empty_output",
            TaskFailureKind::QuotaExhausted => "quota_exhausted",
            TaskFailureKind::PermissionDenied => "permission_denied",
            TaskFailureKind::AgentFailed => "agent_failed",
            TaskFailureKind::InvalidWorktree => "invalid_worktree",
            TaskFailureKind::InvalidProfile => "invalid_profile",
            TaskFailureKind::Unauthenticated => "unauthenticated",
            TaskFailureKind::InvalidPrompt => "invalid_prompt",
            TaskFailureKind::InvalidDuration => "invalid_duration",
            TaskFailureKind::SetupError => "setup_error",
        }
    }
}

/// JSON output contract for `agyp task --json`.
///
/// Stable fields:
/// - `profile`: Target profile name.
/// - `worktree`: Raw worktree path passed on CLI.
/// - `canonical_worktree`: Canonicalized path to worktree (null if validation failed).
/// - `status`: "success" or "error".
/// - `failure_kind`: Top-level failure category (null on success).
/// - `agent_failure_kind`: Original AGY failure category if restoration fails simultaneously (null on success).
/// - `restore_error`: Detail message if Keychain restoration failed (null on success).
/// - `exit_code`: Deprecated backward-compatible exit code (null on timeout or setup error; always serialized).
/// - `agent_exit_code`: Direct exit status of the child AGY process (null on timeout or setup error).
/// - `cli_exit_code`: Exit code returned by `agyp task` (0 on success; 1 on general error; 2 on unauthenticated).
/// - `timed_out`: True if task timed out.
/// - `duration_ms`: Duration of execution in milliseconds.
/// - `stdout`: Captured AGY stdout.
/// - `stderr`: Captured AGY stderr (including restore error detail if restore failed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskJsonOutput {
    pub profile: String,
    pub worktree: String,
    pub canonical_worktree: Option<String>,
    pub status: String,
    pub failure_kind: Option<String>,
    pub agent_failure_kind: Option<String>,
    pub restore_error: Option<String>,
    pub exit_code: Option<i32>,
    pub agent_exit_code: Option<i32>,
    pub cli_exit_code: i32,
    pub timed_out: bool,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
}

pub fn parse_duration(input: &str) -> Result<Duration, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Timeout cannot be empty".to_string());
    }

    if let Some(val_str) = trimmed.strip_suffix("ms") {
        let n: u64 = val_str
            .trim()
            .parse()
            .map_err(|_| format!("Invalid timeout millisecond duration: '{input}'"))?;
        if n == 0 {
            return Err("Timeout must be greater than zero".to_string());
        }
        return Ok(Duration::from_millis(n));
    }

    let (val_str, multiplier) = if let Some(s) = trimmed
        .strip_suffix("seconds")
        .or_else(|| trimmed.strip_suffix("sec"))
        .or_else(|| trimmed.strip_suffix('s'))
    {
        (s, 1)
    } else if let Some(m) = trimmed
        .strip_suffix("minutes")
        .or_else(|| trimmed.strip_suffix("min"))
        .or_else(|| trimmed.strip_suffix('m'))
    {
        (m, 60)
    } else if let Some(h) = trimmed
        .strip_suffix("hours")
        .or_else(|| trimmed.strip_suffix("hr"))
        .or_else(|| trimmed.strip_suffix('h'))
    {
        (h, 3600)
    } else {
        (trimmed, 1)
    };

    let n: u64 = val_str
        .trim()
        .parse()
        .map_err(|_| format!("Invalid timeout duration: '{input}'"))?;
    if n == 0 {
        return Err("Timeout must be greater than zero".to_string());
    }
    Ok(Duration::from_secs(n.saturating_mul(multiplier)))
}

pub fn validate_linked_worktree(path: &Path) -> Result<PathBuf, String> {
    if !path.exists() {
        return Err(format!("Worktree path does not exist: {}", path.display()));
    }
    reject_symlink(path).map_err(|e| format!("Refusing to access symlinked worktree path: {e}"))?;

    let canonical_path = path
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize worktree path: {e}"))?;

    if !canonical_path.is_dir() {
        return Err(format!(
            "Worktree path is not a directory: {}",
            canonical_path.display()
        ));
    }

    let dot_git = canonical_path.join(".git");
    if !dot_git.exists() {
        return Err(format!(
            "Not a git worktree (no .git found): {}",
            canonical_path.display()
        ));
    }
    reject_symlink(&dot_git).map_err(|e| format!("Worktree .git cannot be a symlink: {e}"))?;

    if dot_git.is_dir() {
        return Err(format!(
            "Worktree path is the main repository checkout, not a linked Git worktree: {}",
            canonical_path.display()
        ));
    }
    if !dot_git.is_file() {
        return Err(format!(
            "Worktree .git is not a regular file: {}",
            canonical_path.display()
        ));
    }

    let git_file_content = fs::read_to_string(&dot_git)
        .map_err(|e| format!("Failed to read worktree .git file: {e}"))?;
    let trimmed_git = git_file_content.trim();
    if !trimmed_git.starts_with("gitdir:") {
        return Err(format!(
            "Invalid .git file format for linked worktree (missing gitdir:): {}",
            canonical_path.display()
        ));
    }

    let raw_gitdir = trimmed_git.strip_prefix("gitdir:").unwrap().trim();
    let parsed_gitdir = if Path::new(raw_gitdir).is_relative() {
        canonical_path.join(raw_gitdir)
    } else {
        PathBuf::from(raw_gitdir)
    };
    let canonical_gitdir = parsed_gitdir
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize worktree gitdir path: {e}"))?;

    // Determine common git directory
    let common_dir_output = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(&canonical_path)
        .output()
        .map_err(|e| format!("Failed to query git common dir: {e}"))?;
    if !common_dir_output.status.success() {
        return Err(format!(
            "Git error determining common dir: {}",
            String::from_utf8_lossy(&common_dir_output.stderr).trim()
        ));
    }
    let common_dir_str = String::from_utf8_lossy(&common_dir_output.stdout)
        .trim()
        .to_string();
    let parsed_common_dir = if Path::new(&common_dir_str).is_relative() {
        canonical_path.join(&common_dir_str)
    } else {
        PathBuf::from(&common_dir_str)
    };
    let canonical_common_dir = parsed_common_dir
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize git common dir: {e}"))?;

    // Verify gitdir is worktree-specific (under common git dir's worktrees/), not main .git
    let expected_worktrees_dir = canonical_common_dir.join("worktrees");
    if !canonical_gitdir.starts_with(&expected_worktrees_dir)
        || canonical_gitdir == canonical_common_dir
    {
        return Err(format!(
            "Worktree gitdir is not worktree-specific under common git dir's worktrees/ (got {}): {}",
            canonical_gitdir.display(),
            canonical_path.display()
        ));
    }

    // Require canonical path to be a registered linked worktree from `git worktree list --porcelain`
    let worktree_list_output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(&canonical_path)
        .output()
        .map_err(|e| format!("Failed to list git worktrees: {e}"))?;
    if !worktree_list_output.status.success() {
        return Err(format!(
            "Git error listing worktrees: {}",
            String::from_utf8_lossy(&worktree_list_output.stderr).trim()
        ));
    }

    let worktree_list_str = String::from_utf8_lossy(&worktree_list_output.stdout);
    let mut found_linked = false;
    let mut is_first = true;
    for line in worktree_list_str.lines() {
        if let Some(wt_path_str) = line.strip_prefix("worktree ") {
            let wt_path = PathBuf::from(wt_path_str.trim());
            if let Ok(cwt) = wt_path.canonicalize() {
                if cwt == canonical_path {
                    if !is_first {
                        found_linked = true;
                    }
                    break;
                }
            }
            is_first = false;
        }
    }

    if !found_linked {
        return Err(format!(
            "Worktree is not a registered linked worktree in git: {}",
            canonical_path.display()
        ));
    }

    // Verify it is the exact Git toplevel
    let git_toplevel_output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&canonical_path)
        .output()
        .map_err(|e| format!("Failed to verify Git toplevel: {e}"))?;

    if !git_toplevel_output.status.success() {
        return Err(format!(
            "Git error determining toplevel: {}",
            String::from_utf8_lossy(&git_toplevel_output.stderr).trim()
        ));
    }

    let git_toplevel_str = String::from_utf8_lossy(&git_toplevel_output.stdout)
        .trim()
        .to_string();
    let git_toplevel = PathBuf::from(&git_toplevel_str)
        .canonicalize()
        .map_err(|e| format!("Failed to canonicalize git toplevel path: {e}"))?;

    if git_toplevel != canonical_path {
        return Err(format!(
            "Path is not the exact Git toplevel (expected {}, got {})",
            canonical_path.display(),
            git_toplevel.display()
        ));
    }

    Ok(canonical_path)
}

fn is_quota_exhausted_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();

    // Check for explicit error/status/code prefixes
    let prefixes = ["error:", "status:", "code:", "rpc error:"];
    for p in prefixes {
        if let Some(rest) = lower.strip_prefix(p) {
            let rest = rest.trim_start();
            // Structured check for 429 or resource_exhausted after prefix
            if let Some(after) = rest.strip_prefix("429") {
                let after = after.trim_start();
                if after.is_empty()
                    || after.starts_with(':')
                    || after.starts_with('-')
                    || after.starts_with("resource_exhausted")
                    || after.starts_with("too many requests")
                    || after.starts_with("rate limit")
                    || after.starts_with("quota")
                {
                    return true;
                }
            }
            if let Some(after) = rest.strip_prefix("resource_exhausted") {
                let after = after.trim_start();
                if after.is_empty()
                    || after.starts_with(':')
                    || after.starts_with('-')
                    || after.starts_with("429")
                    || after.starts_with("rate limit")
                    || after.starts_with("quota")
                {
                    return true;
                }
            }
            if rest.starts_with("code = resource_exhausted")
                || rest.starts_with("code = 429")
                || rest.starts_with("individual quota reached")
                || rest.starts_with("quota exceeded")
                || rest.starts_with("quota reached")
                || rest.starts_with("rate limit exceeded")
                || rest.starts_with("rate limit reached")
                || rest.starts_with("insufficient quota")
            {
                return true;
            }
        }
    }

    // Direct patterns without error/status prefix
    if lower.starts_with("429 resource_exhausted")
        || lower.starts_with("429: resource_exhausted")
        || lower.starts_with("resource_exhausted:")
        || lower.starts_with("resource_exhausted: rate limit")
        || lower.starts_with("individual quota reached")
        || lower.starts_with("quota reached")
        || lower.starts_with("quota exceeded")
        || lower.starts_with("rate limit exceeded")
        || lower.starts_with("rate limit reached")
        || lower.starts_with("insufficient quota")
        || lower.starts_with("your quota reached")
    {
        return true;
    }

    false
}

fn is_real_denial_suffix(rest: &str) -> bool {
    let t = rest.trim_start();
    if t.is_empty() {
        return true;
    }
    // Real denial error details follow punctuation (colon, dash, paren, bracket) or specific prepositions
    if t.starts_with(':') || t.starts_with('-') || t.starts_with('(') || t.starts_with('[') {
        return true;
    }
    let lower = t.to_lowercase();
    let prepositions = ["by ", "to ", "for ", "on ", "from ", "during ", "when "];
    prepositions.iter().any(|prep| lower.starts_with(prep))
}

fn is_permission_denied_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();

    // 1. With "error:" prefix
    if let Some(rest) = lower.strip_prefix("error:") {
        let rest = rest.trim_start();
        if let Some(after) = rest.strip_prefix("permission denied") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("permission_denied") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("access denied") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("access_denied") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("tool call rejected") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("permission rejected") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("permissions were rejected") {
            return is_real_denial_suffix(after);
        }
        if let Some(after) = rest.strip_prefix("operation not permitted") {
            return is_real_denial_suffix(after);
        }
    }

    // 2. Direct patterns without "error:" prefix
    if let Some(after) = lower.strip_prefix("permission denied") {
        return is_real_denial_suffix(after);
    }
    if let Some(after) = lower.strip_prefix("permission_denied") {
        return is_real_denial_suffix(after);
    }
    if let Some(after) = lower.strip_prefix("access denied") {
        return is_real_denial_suffix(after);
    }
    if let Some(after) = lower.strip_prefix("access_denied") {
        return is_real_denial_suffix(after);
    }
    if let Some(after) = lower.strip_prefix("tool call rejected") {
        return is_real_denial_suffix(after);
    }
    if let Some(after) = lower.strip_prefix("operation not permitted") {
        return is_real_denial_suffix(after);
    }

    false
}

fn has_quota_exhausted_stderr(stderr: &str) -> bool {
    stderr.lines().any(is_quota_exhausted_line)
}

fn has_permission_denied_stderr(stderr: &str) -> bool {
    stderr.lines().any(is_permission_denied_line)
}

pub fn classify_task_outcome(
    timed_out: bool,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Result<(), TaskFailureKind> {
    if timed_out {
        return Err(TaskFailureKind::Timeout);
    }

    // Finding 3: Classify quota/permission ONLY from original child stderr
    // and only anchored/documented error lines.
    let quota = has_quota_exhausted_stderr(stderr);
    let perm = has_permission_denied_stderr(stderr);

    match exit_code {
        Some(0) => {
            // Exit code 0: check stderr signatures (429 RESOURCE_EXHAUSTED, Individual quota reached)
            if quota {
                Err(TaskFailureKind::QuotaExhausted)
            } else if perm {
                Err(TaskFailureKind::PermissionDenied)
            } else if stdout.trim().is_empty() {
                Err(TaskFailureKind::EmptyOutput)
            } else {
                // Exit-0 stdout prose containing RESOURCE_EXHAUSTED, error: 429, error: permission denied
                // must remain success!
                Ok(())
            }
        }
        _ => {
            // Nonzero status: check stderr signatures, else agent failed
            if quota {
                Err(TaskFailureKind::QuotaExhausted)
            } else if perm {
                Err(TaskFailureKind::PermissionDenied)
            } else {
                Err(TaskFailureKind::AgentFailed)
            }
        }
    }
}

/// Maximum allowed size for a prompt file: 1 MiB (1,048,576 bytes).
/// Capping prompt file size prevents excessive memory consumption and ensures
/// prompt content fits safely within CLI invocation limits.
pub const MAX_PROMPT_FILE_SIZE: u64 = 1024 * 1024; // 1 MiB

#[derive(Debug)]
pub enum ResolvePromptError {
    InvalidPrompt(String),
    Timeout(Duration),
}

#[cfg(unix)]
fn open_prompt_file_unfollowed(path: &Path) -> Result<File, ResolvePromptError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut opts = fs::OpenOptions::new();
    opts.read(true);
    opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    opts.open(path).map_err(|e| {
        if e.raw_os_error() == Some(libc::ELOOP) {
            ResolvePromptError::InvalidPrompt(format!(
                "Prompt file cannot be a symlink: {}",
                path.display()
            ))
        } else if e.kind() == std::io::ErrorKind::NotFound {
            ResolvePromptError::InvalidPrompt(format!(
                "Prompt file does not exist: {}",
                path.display()
            ))
        } else {
            ResolvePromptError::InvalidPrompt(format!(
                "Failed to open prompt file {}: {}",
                path.display(),
                e
            ))
        }
    })
}

#[cfg(not(unix))]
fn open_prompt_file_unfollowed(path: &Path) -> Result<File, ResolvePromptError> {
    fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ResolvePromptError::InvalidPrompt(format!(
                "Prompt file does not exist: {}",
                path.display()
            ))
        } else {
            ResolvePromptError::InvalidPrompt(format!(
                "Failed to open prompt file {}: {}",
                path.display(),
                e
            ))
        }
    })
}

/// Reads prompt content directly from an opened file handle without re-opening or path re-checks.
/// Validates fstat metadata to reject non-regular files (FIFOs, devices, sockets, directories)
/// and enforces `MAX_PROMPT_FILE_SIZE`.
pub fn read_prompt_from_opened_file(file: File, path: &Path) -> Result<String, ResolvePromptError> {
    let meta = file.metadata().map_err(|e| {
        ResolvePromptError::InvalidPrompt(format!(
            "Failed to read prompt file metadata from opened handle for {}: {e}",
            path.display()
        ))
    })?;

    if !meta.is_file() {
        return Err(ResolvePromptError::InvalidPrompt(format!(
            "Prompt file must be a regular file (got FIFO, directory, or special device): {}",
            path.display()
        )));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let ft = meta.file_type();
        if ft.is_fifo() || ft.is_char_device() || ft.is_block_device() || ft.is_socket() {
            return Err(ResolvePromptError::InvalidPrompt(format!(
                "Prompt file must be a regular file (got FIFO or special device): {}",
                path.display()
            )));
        }
    }

    let file_size = meta.len();
    if file_size > MAX_PROMPT_FILE_SIZE {
        return Err(ResolvePromptError::InvalidPrompt(format!(
            "Prompt file size ({} bytes) exceeds maximum allowed limit of {} bytes: {}",
            file_size,
            MAX_PROMPT_FILE_SIZE,
            path.display()
        )));
    }

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = file.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
        }
    }

    let mut content = String::with_capacity(file_size as usize);
    let mut limited = file.take(MAX_PROMPT_FILE_SIZE + 1);
    limited.read_to_string(&mut content).map_err(|e| {
        ResolvePromptError::InvalidPrompt(format!(
            "Failed to read prompt file {}: {e}",
            path.display()
        ))
    })?;

    if content.len() as u64 > MAX_PROMPT_FILE_SIZE {
        return Err(ResolvePromptError::InvalidPrompt(format!(
            "Prompt file content exceeded maximum allowed limit of {} bytes: {}",
            MAX_PROMPT_FILE_SIZE,
            path.display()
        )));
    }

    Ok(content)
}

/// Safely opens and reads a prompt file from a single handle, bounded by the task deadline timeout.
pub fn read_prompt_file_safely(
    path: &Path,
    timeout: Option<Duration>,
) -> Result<String, ResolvePromptError> {
    if let Some(limit) = timeout {
        if limit.is_zero() {
            return Err(ResolvePromptError::Timeout(limit));
        }
    }

    let path_buf = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let res = (|| -> Result<String, ResolvePromptError> {
            let file = open_prompt_file_unfollowed(&path_buf)?;
            read_prompt_from_opened_file(file, &path_buf)
        })();
        let _ = tx.send(res);
    });

    match timeout {
        Some(limit) => match rx.recv_timeout(limit) {
            Ok(res) => res,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(ResolvePromptError::Timeout(limit))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(
                ResolvePromptError::InvalidPrompt("Prompt file reader disconnected".to_string()),
            ),
        },
        None => match rx.recv() {
            Ok(res) => res,
            Err(_) => Err(ResolvePromptError::InvalidPrompt(
                "Prompt file reader disconnected".to_string(),
            )),
        },
    }
}

pub fn resolve_task_prompt(prompt_file: Option<&Path>) -> Result<String, String> {
    resolve_task_prompt_with_timeout(prompt_file, None).map_err(|e| match e {
        ResolvePromptError::InvalidPrompt(msg) => msg,
        ResolvePromptError::Timeout(d) => format!("Timed out after {:?}", d),
    })
}

pub fn resolve_task_prompt_with_timeout(
    prompt_file: Option<&Path>,
    timeout: Option<Duration>,
) -> Result<String, ResolvePromptError> {
    let content = match prompt_file {
        Some(file) => read_prompt_file_safely(file, timeout)?,
        None => {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let mut stdin_buf = String::new();
                let res = io::stdin().read_to_string(&mut stdin_buf);
                let _ = tx.send((res, stdin_buf));
            });

            let (res, stdin_buf) = match timeout {
                Some(limit) => match rx.recv_timeout(limit) {
                    Ok(val) => val,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        return Err(ResolvePromptError::Timeout(limit));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(ResolvePromptError::InvalidPrompt(
                            "Stdin reader disconnected".to_string(),
                        ));
                    }
                },
                None => match rx.recv() {
                    Ok(val) => val,
                    Err(_) => {
                        return Err(ResolvePromptError::InvalidPrompt(
                            "Stdin reader disconnected".to_string(),
                        ));
                    }
                },
            };

            res.map_err(|e| {
                ResolvePromptError::InvalidPrompt(format!("Failed to read prompt from stdin: {e}"))
            })?;
            stdin_buf
        }
    };

    if content.trim().is_empty() {
        return Err(ResolvePromptError::InvalidPrompt(
            "Prompt content cannot be empty.".to_string(),
        ));
    }
    Ok(content)
}

fn emit_json_result(output: &TaskJsonOutput) {
    println!("{}", serde_json::to_string_pretty(output).unwrap());
}

fn make_setup_error_output(
    profile: &str,
    raw_worktree: &str,
    canonical_worktree: Option<&Path>,
    failure_kind: TaskFailureKind,
    error_msg: &str,
    exit_code: i32,
    start_instant: std::time::Instant,
) -> (Result<(), i32>, TaskJsonOutput) {
    let json_res = TaskJsonOutput {
        profile: profile.to_string(),
        worktree: raw_worktree.to_string(),
        canonical_worktree: canonical_worktree.map(|p| p.display().to_string()),
        status: "error".to_string(),
        failure_kind: Some(failure_kind.as_str().to_string()),
        agent_failure_kind: None,
        restore_error: None,
        exit_code: None,
        agent_exit_code: None,
        cli_exit_code: exit_code,
        timed_out: false,
        duration_ms: start_instant.elapsed().as_millis() as u64,
        stdout: String::new(),
        stderr: error_msg.to_string(),
    };
    (Err(exit_code), json_res)
}

fn make_timeout_error_output(
    profile: &str,
    raw_worktree: &str,
    canonical_worktree: Option<&Path>,
    timeout: Duration,
    start_instant: std::time::Instant,
) -> (Result<(), i32>, TaskJsonOutput) {
    let msg = format!("Task timed out after {:?}", timeout);
    let json_res = TaskJsonOutput {
        profile: profile.to_string(),
        worktree: raw_worktree.to_string(),
        canonical_worktree: canonical_worktree.map(|p| p.display().to_string()),
        status: "error".to_string(),
        failure_kind: Some(TaskFailureKind::Timeout.as_str().to_string()),
        agent_failure_kind: None,
        restore_error: None,
        exit_code: None,
        agent_exit_code: None,
        cli_exit_code: 1,
        timed_out: true,
        duration_ms: start_instant.elapsed().as_millis() as u64,
        stdout: String::new(),
        stderr: msg,
    };
    (Err(1), json_res)
}

#[allow(clippy::too_many_arguments)]
fn execute_task_impl<B: crate::keychain::KeychainBackend>(
    profile_name: &str,
    worktree_path: &Path,
    prompt_file: Option<&Path>,
    timeout_str: Option<&str>,
    model: Option<&str>,
    skip_permissions: bool,
    backend: B,
) -> (Result<(), i32>, TaskJsonOutput) {
    let start_instant = std::time::Instant::now();
    let raw_worktree = worktree_path.to_string_lossy().to_string();

    let timeout_duration = match timeout_str {
        Some(s) => match parse_duration(s) {
            Ok(d) => d,
            Err(e) => {
                return make_setup_error_output(
                    profile_name,
                    &raw_worktree,
                    None,
                    TaskFailureKind::InvalidDuration,
                    &e,
                    1,
                    start_instant,
                );
            }
        },
        None => Duration::from_secs(20 * 60),
    };

    let config_path = ProfileRegistry::get_config_file_path();
    let reg = match ProfileRegistry::load_or_default(&config_path) {
        Ok(r) => r,
        Err(e) => {
            return make_setup_error_output(
                profile_name,
                &raw_worktree,
                None,
                TaskFailureKind::SetupError,
                &format!("Failed to load registry: {e}"),
                1,
                start_instant,
            );
        }
    };

    let profile = match reg.resolve_profile(profile_name) {
        Some(p) => p.clone(),
        None => {
            return make_setup_error_output(
                profile_name,
                &raw_worktree,
                None,
                TaskFailureKind::InvalidProfile,
                &format!("Profile or alias '{profile_name}' not found."),
                1,
                start_instant,
            );
        }
    };

    let keychain_token_file = profile.dir.join("keychain_token.json");
    if !keychain_token_file.is_file() {
        let has_legacy_creds = profile.dir.join("oauth_creds.json").is_file()
            || profile.dir.join("jetski-standalone-oauth-token").is_file();
        let guidance = if has_legacy_creds {
            format!(
                "Profile '{}' contains legacy credentials but lacks 'keychain_token.json'. Please run 'agyp login {}' to authenticate the profile for task execution.",
                profile.name, profile.name
            )
        } else {
            format!(
                "Profile '{}' is not authenticated. Please run 'agyp login {}' first.",
                profile.name, profile.name
            )
        };
        return make_setup_error_output(
            &profile.name,
            &raw_worktree,
            None,
            TaskFailureKind::Unauthenticated,
            &guidance,
            2,
            start_instant,
        );
    }

    let canonical_worktree = match validate_linked_worktree(worktree_path) {
        Ok(cw) => cw,
        Err(e) => {
            return make_setup_error_output(
                &profile.name,
                &raw_worktree,
                None,
                TaskFailureKind::InvalidWorktree,
                &e,
                1,
                start_instant,
            );
        }
    };

    let elapsed = start_instant.elapsed();
    if elapsed >= timeout_duration {
        return make_timeout_error_output(
            &profile.name,
            &raw_worktree,
            Some(&canonical_worktree),
            timeout_duration,
            start_instant,
        );
    }
    let remaining_prompt_timeout = timeout_duration - elapsed;

    let prompt_content =
        match resolve_task_prompt_with_timeout(prompt_file, Some(remaining_prompt_timeout)) {
            Ok(p) => p,
            Err(ResolvePromptError::Timeout(_)) => {
                return make_timeout_error_output(
                    &profile.name,
                    &raw_worktree,
                    Some(&canonical_worktree),
                    timeout_duration,
                    start_instant,
                );
            }
            Err(ResolvePromptError::InvalidPrompt(e)) => {
                return make_setup_error_output(
                    &profile.name,
                    &raw_worktree,
                    Some(&canonical_worktree),
                    TaskFailureKind::InvalidPrompt,
                    &e,
                    1,
                    start_instant,
                );
            }
        };

    let mut args = Vec::new();
    if skip_permissions {
        args.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(m) = model {
        args.push(format!("--model={}", m));
    }
    args.push("-p".to_string());
    args.push(prompt_content);

    let _ = ProfileRegistry::update(&config_path, |reg| {
        if let Some(p) = reg.profiles.get_mut(&profile.name) {
            p.last_used_at = Some(chrono::Utc::now());
        }
        Ok(())
    });

    let mut cmd = match build_agy_command(&profile.dir, &args) {
        Ok(c) => c,
        Err(e) => {
            return make_setup_error_output(
                &profile.name,
                &raw_worktree,
                Some(&canonical_worktree),
                TaskFailureKind::SetupError,
                &e,
                127,
                start_instant,
            );
        }
    };

    cmd.current_dir(&canonical_worktree);

    let elapsed_lock = start_instant.elapsed();
    if elapsed_lock >= timeout_duration {
        return make_timeout_error_output(
            &profile.name,
            &raw_worktree,
            Some(&canonical_worktree),
            timeout_duration,
            start_instant,
        );
    }
    let remaining_lock_timeout = timeout_duration - elapsed_lock;

    let lock_path =
        crate::security::profile_lock_path(&ProfileRegistry::get_profiles_dir(), &profile.name);
    let process_lock = match ProcessLock::acquire_with_timeout(&lock_path, remaining_lock_timeout) {
        Ok(l) => l,
        Err(e) if e.contains("Timed out") => {
            return make_timeout_error_output(
                &profile.name,
                &raw_worktree,
                Some(&canonical_worktree),
                timeout_duration,
                start_instant,
            );
        }
        Err(e) => {
            let err_msg = format!("Failed to serialize AGY profile access: {e}");
            return make_setup_error_output(
                &profile.name,
                &raw_worktree,
                Some(&canonical_worktree),
                TaskFailureKind::SetupError,
                &err_msg,
                1,
                start_instant,
            );
        }
    };

    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let profile_dir = match validate_profile_dir_under(&profile.dir, &profiles_root) {
        Ok(d) => d,
        Err(e) => {
            let err_msg = format!("Invalid profile directory: {e}");
            return make_setup_error_output(
                &profile.name,
                &raw_worktree,
                Some(&canonical_worktree),
                TaskFailureKind::SetupError,
                &err_msg,
                1,
                start_instant,
            );
        }
    };

    let keychain_guard = match activate_profile(
        &profile_dir,
        backend,
        std::time::Instant::now() + PROFILE_KEYCHAIN_SETUP_TIMEOUT,
        process_lock,
    ) {
        Ok((g, profile_home)) => {
            apply_profile_home(&mut cmd, profile_home.as_deref());
            g
        }
        Err(e) => {
            let err_msg = format!(
                "Failed to configure keychain for profile '{}': {}",
                profile.name, e
            );
            return make_setup_error_output(
                &profile.name,
                &raw_worktree,
                Some(&canonical_worktree),
                TaskFailureKind::SetupError,
                &err_msg,
                1,
                start_instant,
            );
        }
    };

    let elapsed_before_exec = start_instant.elapsed();
    let (output_result, timed_out_before_exec) = if elapsed_before_exec >= timeout_duration {
        (
            Ok(crate::agy::CapturedOutput {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out: true,
            }),
            true,
        )
    } else {
        let remaining_exec_timeout = timeout_duration - elapsed_before_exec;
        (
            run_command_captured_interruptibly_with_timeout(&mut cmd, Some(remaining_exec_timeout)),
            false,
        )
    };

    // On all paths after Keychain activation, restoration must be attempted before returning
    let restore_result = keychain_guard.sync_and_restore();
    let duration_ms = start_instant.elapsed().as_millis() as u64;

    let (output, exec_err) = match output_result {
        Ok(out) => (out, None),
        Err(e) => (
            crate::agy::CapturedOutput {
                status: None,
                stdout: Vec::new(),
                stderr: e.as_bytes().to_vec(),
                timed_out: false,
            },
            Some(e),
        ),
    };

    let stdout_str = String::from_utf8_lossy(&output.stdout).to_string();
    let original_child_stderr = String::from_utf8_lossy(&output.stderr).to_string();

    let is_timed_out = output.timed_out || timed_out_before_exec;
    let raw_exit_code = output.status.and_then(|s| s.code());

    // Finding 4: Classify child outcome BEFORE appending restoration error!
    let classification = if exec_err.is_some() {
        Err(TaskFailureKind::AgentFailed)
    } else {
        classify_task_outcome(
            is_timed_out,
            raw_exit_code,
            &stdout_str,
            &original_child_stderr,
        )
    };

    let restore_error_str = restore_result.as_ref().err().cloned();
    let mut combined_stderr = original_child_stderr;
    if let Some(ref re) = restore_error_str {
        if !combined_stderr.is_empty() {
            combined_stderr.push('\n');
        }
        combined_stderr.push_str(&format!("Keychain restoration error: {re}"));
    }

    // Finding 4 & 5: Precedence: If restore failed, failure_kind is setup_error.
    // Never let restore text populate agent_failure_kind!
    let (failure_kind, agent_failure_kind) = match (&classification, &restore_error_str) {
        (_, Some(_)) => {
            let agent_kind = match classification {
                Ok(_) => None,
                Err(k) => Some(k.as_str().to_string()),
            };
            (Some(TaskFailureKind::SetupError), agent_kind)
        }
        (Ok(_), None) => (None, None),
        (Err(k), None) => (Some(*k), None),
    };

    let status_str = if failure_kind.is_none() {
        "success"
    } else {
        "error"
    };

    let agent_exit_code = if is_timed_out { None } else { raw_exit_code };

    let cli_exit_code = if failure_kind.is_none() {
        0
    } else if restore_error_str.is_some() {
        1
    } else {
        match failure_kind {
            Some(TaskFailureKind::Unauthenticated) => 2,
            Some(TaskFailureKind::SetupError) => 1,
            _ => agent_exit_code.filter(|&c| c != 0).unwrap_or(1),
        }
    };

    let json_res = TaskJsonOutput {
        profile: profile.name.clone(),
        worktree: raw_worktree,
        canonical_worktree: Some(canonical_worktree.display().to_string()),
        status: status_str.to_string(),
        failure_kind: failure_kind.map(|k| k.as_str().to_string()),
        agent_failure_kind,
        restore_error: restore_error_str,
        exit_code: agent_exit_code,
        agent_exit_code,
        cli_exit_code,
        timed_out: is_timed_out,
        duration_ms,
        stdout: stdout_str,
        stderr: combined_stderr,
    };

    let res_code = if failure_kind.is_none() {
        Ok(())
    } else {
        Err(cli_exit_code)
    };

    (res_code, json_res)
}

fn print_task_output(
    res: &TaskJsonOutput,
    json_output: bool,
    failure_kind: Option<TaskFailureKind>,
) {
    if json_output {
        emit_json_result(res);
    } else {
        if !res.stdout.is_empty() {
            print!("{}", res.stdout);
        }
        if !res.stderr.is_empty() {
            eprint!("{}", res.stderr);
            if !res.stderr.ends_with('\n') {
                eprintln!();
            }
        }
        if let Some(ref re) = res.restore_error {
            eprintln!("CRITICAL ERROR [setup_error]: Keychain restoration failed: {re}");
        }
        if let Some(kind) = failure_kind {
            let msg = match kind {
                TaskFailureKind::Timeout => format!("Task timed out after {}ms", res.duration_ms),
                TaskFailureKind::EmptyOutput => {
                    "AGY exited 0 but produced empty stdout".to_string()
                }
                TaskFailureKind::QuotaExhausted => {
                    "Rate limit or quota exhausted (429)".to_string()
                }
                TaskFailureKind::PermissionDenied => {
                    "Permission denied or tool call rejected".to_string()
                }
                TaskFailureKind::AgentFailed => {
                    format!(
                        "AGY command failed with exit code {:?}",
                        res.agent_exit_code
                    )
                }
                TaskFailureKind::SetupError => "Setup or keychain restore error".to_string(),
                _ => kind.as_str().to_string(),
            };
            eprintln!("Error [{}]: {}", kind.as_str(), msg);
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_task(
    profile_name: &str,
    worktree_path: &Path,
    prompt_file: Option<&Path>,
    timeout_str: Option<&str>,
    model: Option<&str>,
    skip_permissions: bool,
    json_output: bool,
) -> Result<(), i32> {
    let (code, output) = execute_task_impl(
        profile_name,
        worktree_path,
        prompt_file,
        timeout_str,
        model,
        skip_permissions,
        crate::keychain::SystemKeychainBackend,
    );
    let fk = output.failure_kind.as_deref().and_then(|k| match k {
        "timeout" => Some(TaskFailureKind::Timeout),
        "empty_output" => Some(TaskFailureKind::EmptyOutput),
        "quota_exhausted" => Some(TaskFailureKind::QuotaExhausted),
        "permission_denied" => Some(TaskFailureKind::PermissionDenied),
        "agent_failed" => Some(TaskFailureKind::AgentFailed),
        "setup_error" => Some(TaskFailureKind::SetupError),
        "invalid_worktree" => Some(TaskFailureKind::InvalidWorktree),
        "invalid_profile" => Some(TaskFailureKind::InvalidProfile),
        "unauthenticated" => Some(TaskFailureKind::Unauthenticated),
        "invalid_prompt" => Some(TaskFailureKind::InvalidPrompt),
        "invalid_duration" => Some(TaskFailureKind::InvalidDuration),
        _ => None,
    });
    print_task_output(&output, json_output, fk);
    code
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn cmd_task_with_backend<B: crate::keychain::KeychainBackend>(
    profile_name: &str,
    worktree_path: &Path,
    prompt_file: Option<&Path>,
    timeout_str: Option<&str>,
    model: Option<&str>,
    skip_permissions: bool,
    json_output: bool,
    backend: B,
) -> Result<(), i32> {
    let (code, output) = execute_task_impl(
        profile_name,
        worktree_path,
        prompt_file,
        timeout_str,
        model,
        skip_permissions,
        backend,
    );
    let fk = output.failure_kind.as_deref().and_then(|k| match k {
        "timeout" => Some(TaskFailureKind::Timeout),
        "empty_output" => Some(TaskFailureKind::EmptyOutput),
        "quota_exhausted" => Some(TaskFailureKind::QuotaExhausted),
        "permission_denied" => Some(TaskFailureKind::PermissionDenied),
        "agent_failed" => Some(TaskFailureKind::AgentFailed),
        "setup_error" => Some(TaskFailureKind::SetupError),
        "invalid_worktree" => Some(TaskFailureKind::InvalidWorktree),
        "invalid_profile" => Some(TaskFailureKind::InvalidProfile),
        "unauthenticated" => Some(TaskFailureKind::Unauthenticated),
        "invalid_prompt" => Some(TaskFailureKind::InvalidPrompt),
        "invalid_duration" => Some(TaskFailureKind::InvalidDuration),
        _ => None,
    });
    print_task_output(&output, json_output, fk);
    code
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn execute_task_with_backend<B: crate::keychain::KeychainBackend>(
    profile_name: &str,
    worktree_path: &Path,
    prompt_file: Option<&Path>,
    timeout_str: Option<&str>,
    model: Option<&str>,
    skip_permissions: bool,
    backend: B,
) -> (Result<(), i32>, TaskJsonOutput) {
    execute_task_impl(
        profile_name,
        worktree_path,
        prompt_file,
        timeout_str,
        model,
        skip_permissions,
        backend,
    )
}
