//! Running the Codex CLI under a profile and interpreting `codex exec --json` output.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::commands::task::TaskFailureKind;

/// Unroutable endpoint Codex is pointed at for token refresh and revocation. Together with the
/// refresh-token sentinel this guarantees codexp never rotates or revokes an upstream token.
pub const DISABLED_OAUTH_URL: &str = "http://127.0.0.1:9/codexp-oauth-disabled";

/// Credentials in the caller's environment that would make Codex bypass the profile's auth.json.
const STRIPPED_ENV: &[&str] = &[
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "CODEX_ACCESS_TOKEN",
    "OPENAI_BASE_URL",
];

pub fn locate_codex_binary() -> Result<PathBuf, String> {
    if let Ok(path) = env::var("CODEXP_CODEX_BIN") {
        return Ok(PathBuf::from(path));
    }
    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let candidate = dir.join("codex");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    let mut fallbacks = vec![
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ];
    if let Some(home) = dirs::home_dir() {
        fallbacks.insert(0, home.join(".local/bin/codex"));
    }
    fallbacks
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| "Could not find 'codex' binary in PATH or standard locations.".to_string())
}

/// `-c` overrides every codexp-launched Codex gets, whatever the shared config says.
pub fn enforced_config_args() -> Vec<String> {
    [
        "model_provider=\"openai\"",
        "cli_auth_credentials_store=\"file\"",
        "forced_login_method=\"chatgpt\"",
    ]
    .iter()
    .flat_map(|kv| ["-c".to_string(), kv.to_string()])
    .collect()
}

/// A `codex` command bound to one profile home. `args` follow the binary; callers place
/// `enforced_config_args()` where the subcommand accepts them.
pub fn build_codex_command(home: &Path, args: &[String]) -> Result<Command, String> {
    let mut cmd = Command::new(locate_codex_binary()?);
    cmd.args(args)
        .env("CODEX_HOME", home)
        .env("CODEX_SQLITE_HOME", super::home::sqlite_home()?)
        .env("CODEX_REFRESH_TOKEN_URL_OVERRIDE", DISABLED_OAUTH_URL)
        .env("CODEX_REVOKE_TOKEN_URL_OVERRIDE", DISABLED_OAUTH_URL);
    for key in STRIPPED_ENV {
        cmd.env_remove(key);
    }
    Ok(cmd)
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ExecSummary {
    pub thread_id: Option<String>,
    pub final_message: Option<String>,
    pub errors: Vec<String>,
    pub usage: Option<Value>,
    pub turn_completed: bool,
}

fn error_message(v: &Value) -> Option<String> {
    v.get("message")
        .and_then(|m| m.as_str())
        .map(str::to_string)
        .or_else(|| v.as_str().map(str::to_string))
        .or_else(|| (!v.is_null()).then(|| v.to_string()))
}

/// Parses the JSONL event stream of `codex exec --json`. Unknown events and non-JSON lines are
/// ignored so newer Codex versions degrade gracefully.
pub fn parse_exec_events(stdout: &str) -> ExecSummary {
    let mut summary = ExecSummary::default();
    for line in stdout.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match event.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "thread.started" => {
                if let Some(id) = event.get("thread_id").and_then(|v| v.as_str()) {
                    summary.thread_id = Some(id.to_string());
                }
            }
            "turn.completed" => {
                summary.turn_completed = true;
                if let Some(usage) = event.get("usage") {
                    summary.usage = Some(usage.clone());
                }
            }
            "turn.failed" => {
                if let Some(msg) = event.get("error").and_then(error_message) {
                    summary.errors.push(msg);
                }
            }
            "error" => {
                if let Some(msg) = error_message(&event) {
                    summary.errors.push(msg);
                }
            }
            "item.completed" => {
                let Some(item) = event.get("item") else {
                    continue;
                };
                let kind = item
                    .get("type")
                    .or_else(|| item.get("item_type"))
                    .and_then(|v| v.as_str());
                if kind == Some("agent_message") {
                    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                        summary.final_message = Some(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    summary
}

fn is_auth_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "could not be refreshed",
        "refresh token",
        "refresh_token",
        "401 unauthorized",
        "status 401",
        "unauthorized",
        "please log out and sign in again",
        "sign in again",
        "token_expired",
        "auth_recovery_unavailable",
        "codexp-oauth-disabled",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

fn is_quota_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    [
        "usage limit",
        "usage_limit_reached",
        "usage_limit_exceeded",
        "rate_limit_reached",
        "rate limit",
        "429",
        "too many requests",
        "quota",
        "credits_depleted",
    ]
    .iter()
    .any(|p| lower.contains(p))
}

/// Classifies a finished `codex exec --json` run. Only structured error events and Codex's own
/// stderr are inspected, never the agent's prose, so a task that merely talks about rate limits
/// is not mistaken for one that hit them.
pub fn classify_exec(
    timed_out: bool,
    exit_code: Option<i32>,
    summary: &ExecSummary,
    stderr: &str,
) -> Result<(), TaskFailureKind> {
    if timed_out {
        return Err(TaskFailureKind::Timeout);
    }
    let signals: Vec<&str> = summary
        .errors
        .iter()
        .map(String::as_str)
        .chain(stderr.lines())
        .collect();
    let succeeded = exit_code == Some(0) && summary.errors.is_empty();
    if !succeeded {
        if signals.iter().any(|s| is_quota_failure(s)) {
            return Err(TaskFailureKind::QuotaExhausted);
        }
        if signals.iter().any(|s| is_auth_failure(s)) {
            return Err(TaskFailureKind::Unauthenticated);
        }
        return Err(TaskFailureKind::AgentFailed);
    }
    match &summary.final_message {
        Some(m) if !m.trim().is_empty() => Ok(()),
        _ => Err(TaskFailureKind::EmptyOutput),
    }
}
