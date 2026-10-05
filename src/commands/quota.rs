#![cfg_attr(test, allow(dead_code))]

use std::collections::HashMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::agy::{
    build_agy_command, check_profile_auth, run_command_captured_interruptibly_with_timeout,
    CapturedOutput,
};
use crate::commands::task::parse_duration;
use crate::config::ProfileRegistry;
use crate::keychain::{
    activate_profile, apply_profile_home, KeychainBackend, SystemKeychainBackend,
};
use crate::security::{profile_lock_path, validate_profile_dir_under, ProcessLock};

pub const QUOTA_PROBE_PROMPT: &str = "Respond with PONG";
pub const QUOTA_PROBE_EXPECTED: &str = "PONG";
pub const QUOTA_CACHE_FILENAME: &str = "quota-cache.json";
pub const QUOTA_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_QUOTA_TIMEOUT: Duration = Duration::from_secs(30);
const RESTORE_EMERGENCY_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    Available,
    QuotaExhausted,
    Unauthenticated,
    Unknown,
    Error,
    Unsupported,
}

impl QuotaStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            QuotaStatus::Available => "available",
            QuotaStatus::QuotaExhausted => "quota_exhausted",
            QuotaStatus::Unauthenticated => "unauthenticated",
            QuotaStatus::Unknown => "unknown",
            QuotaStatus::Error => "error",
            QuotaStatus::Unsupported => "unsupported",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedQuotaEntry {
    pub profile: String,
    pub email: Option<String>,
    pub status: String,
    pub observed_at: String,
    pub detail: Option<String>,
    pub agent_exit_code: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaCache {
    pub version: u32,
    pub entries: HashMap<String, CachedQuotaEntry>,
}

impl Default for QuotaCache {
    fn default() -> Self {
        Self {
            version: 1,
            entries: HashMap::new(),
        }
    }
}

pub fn load_quota_cache(cache_path: &Path) -> QuotaCache {
    let metadata = match fs::symlink_metadata(cache_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return QuotaCache::default();
        }
        Err(_) => return QuotaCache::default(),
    };

    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return QuotaCache::default();
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            let mut permissions = metadata.permissions();
            permissions.set_mode(0o600);
            if fs::set_permissions(cache_path, permissions).is_err() {
                return QuotaCache::default();
            }
        }
    }

    match fs::read_to_string(cache_path) {
        Ok(content) => match serde_json::from_str::<QuotaCache>(&content) {
            Ok(cache) if cache.version == 1 => cache,
            _ => QuotaCache::default(),
        },
        Err(_) => QuotaCache::default(),
    }
}

pub fn save_quota_cache(cache_path: &Path, cache: &QuotaCache) -> Result<(), String> {
    let parent = cache_path.parent().unwrap_or_else(|| Path::new(""));
    if !parent.as_os_str().is_empty() {
        crate::security::secure_dir(parent)
            .map_err(|e| format!("Failed to create private parent dir: {e}"))?;
    }

    let file_name = cache_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(QUOTA_CACHE_FILENAME);
    let tmp_path = if parent.as_os_str().is_empty() {
        PathBuf::from(format!("{}.tmp.{}", file_name, std::process::id()))
    } else {
        parent.join(format!("{}.tmp.{}", file_name, std::process::id()))
    };

    let content =
        serde_json::to_string_pretty(cache).map_err(|e| format!("Serialization error: {e}"))?;

    if let Ok(metadata) = fs::symlink_metadata(cache_path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("Refusing unsafe quota cache path".to_string());
        }
    }
    if let Ok(metadata) = fs::symlink_metadata(&tmp_path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err("Refusing unsafe quota cache temporary path".to_string());
        }
        fs::remove_file(&tmp_path)
            .map_err(|e| format!("Failed to clear stale quota cache temporary file: {e}"))?;
    }

    let write_and_sync = || -> std::io::Result<()> {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp_path)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        crate::security::set_private_file_permissions(&tmp_path)?;
        Ok(())
    };

    if let Err(e) = write_and_sync() {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("Failed to write temp quota cache: {e}"));
    }

    if let Err(e) = fs::rename(&tmp_path, cache_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(format!("Failed to replace quota cache: {e}"));
    }

    Ok(())
}

pub fn sanitize_detail(msg: &str) -> String {
    let trimmed = msg.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed == "Probe completed successfully"
        || trimmed == "Probe completed without expected output"
        || trimmed == "Authentication failed"
        || trimmed == "Rate limit or quota exhausted (429 / RESOURCE_EXHAUSTED)"
        || trimmed == "Probe terminated by signal or failed to execute"
        || trimmed.starts_with("Probe exited with unrecognized status code ")
    {
        return trimmed.to_string();
    }

    // Quota output is deliberately enum-like. Never forward arbitrary backend,
    // filesystem, process, or Keychain text: it may contain credentials in a
    // representation that a heuristic redactor does not recognize.
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        "Probe timed out.".to_string()
    } else if lower.contains("profile or alias") && lower.contains("not found") {
        "Profile or alias not found.".to_string()
    } else if lower.contains("no registered profiles") {
        "No registered profiles found to check.".to_string()
    } else if lower.contains("cannot specify both") {
        "Cannot specify both --profile and --all.".to_string()
    } else if lower.contains("exactly one") {
        "Exactly one of --profile or --all is required.".to_string()
    } else if lower.contains("invalid profile directory") {
        "Invalid profile directory.".to_string()
    } else if lower.contains("not authenticated") || lower.contains("missing stored credential") {
        "Profile is not authenticated.".to_string()
    } else if lower.contains("keychain") || lower.contains("credential") {
        "Credential transaction failed.".to_string()
    } else if lower.contains("cache") {
        "Quota cache operation failed.".to_string()
    } else if lower.contains("lock") {
        "Could not acquire the profile transaction lock.".to_string()
    } else if lower.contains("agy") || lower.contains("probe") || lower.contains("process") {
        "Quota probe failed.".to_string()
    } else {
        "Quota status check failed.".to_string()
    }
}

fn is_quota_exhausted_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    let prefixes = ["error:", "status:", "code:", "rpc error:"];
    for p in prefixes {
        if let Some(rest) = lower.strip_prefix(p) {
            let rest = rest.trim_start();
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

fn is_unauthenticated_line(line: &str) -> bool {
    let lower = line.trim().to_lowercase();
    let prefixes = ["error:", "status:", "code:", "rpc error:"];
    for p in prefixes {
        if let Some(rest) = lower.strip_prefix(p) {
            let rest = rest.trim_start();
            if rest.starts_with("unauthenticated")
                || rest.starts_with("unauthorized")
                || rest.starts_with("401")
                || rest.starts_with("invalid_grant")
                || rest.starts_with("authentication failed")
                || rest.starts_with("token expired")
            {
                return true;
            }
        }
    }
    if lower.starts_with("unauthenticated:")
        || lower.starts_with("unauthorized:")
        || lower.starts_with("401 unauthorized")
        || lower.starts_with("401: unauthorized")
        || lower.contains("invalid_grant")
        || lower.starts_with("authentication failed")
        || lower.starts_with("failed to authenticate")
    {
        return true;
    }
    false
}

pub fn classify_probe_outcome(
    timed_out: bool,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> (QuotaStatus, Option<String>) {
    if timed_out {
        return (QuotaStatus::Error, Some("Probe timed out".to_string()));
    }

    let has_quota = stderr.lines().any(is_quota_exhausted_line);
    let has_auth = stderr.lines().any(is_unauthenticated_line);

    if has_quota {
        return (
            QuotaStatus::QuotaExhausted,
            Some("Rate limit or quota exhausted (429 / RESOURCE_EXHAUSTED)".to_string()),
        );
    }

    if has_auth {
        return (
            QuotaStatus::Unauthenticated,
            Some("Authentication failed".to_string()),
        );
    }

    match exit_code {
        Some(0) => {
            if stdout.trim().eq_ignore_ascii_case(QUOTA_PROBE_EXPECTED) && stderr.trim().is_empty()
            {
                (
                    QuotaStatus::Available,
                    Some("Probe completed successfully".to_string()),
                )
            } else {
                (
                    QuotaStatus::Unknown,
                    Some("Probe completed without expected output".to_string()),
                )
            }
        }
        Some(code) => (
            QuotaStatus::Unknown,
            Some(format!("Probe exited with unrecognized status code {code}")),
        ),
        None => (
            QuotaStatus::Error,
            Some("Probe terminated by signal or failed to execute".to_string()),
        ),
    }
}

pub fn calculate_quota_exit_code(statuses: &[QuotaStatus]) -> i32 {
    if statuses.is_empty() {
        return 0;
    }

    let has_error = statuses.iter().any(|s| {
        matches!(
            s,
            QuotaStatus::Error | QuotaStatus::Unknown | QuotaStatus::Unsupported
        )
    });
    if has_error {
        return 1;
    }

    let has_quota_or_unauth = statuses.iter().any(|s| {
        matches!(
            s,
            QuotaStatus::QuotaExhausted | QuotaStatus::Unauthenticated
        )
    });
    if has_quota_or_unauth {
        return 2;
    }

    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaJsonOutput {
    pub schema_version: u32,
    pub checked_at: String,
    pub source: String,
    pub results: Vec<serde_json::Value>,
    pub cli_exit_code: i32,
}

pub trait QuotaProbeRunner: Send + Sync {
    /// `profile_home` is the isolated HOME the AGY probe must run with, if any.
    fn run_probe(
        &self,
        profile_dir: &Path,
        profile_home: Option<&Path>,
        timeout: Option<Duration>,
    ) -> Result<CapturedOutput, String>;
}

#[derive(Clone, Default)]
pub struct SystemProbeRunner;

impl QuotaProbeRunner for SystemProbeRunner {
    fn run_probe(
        &self,
        profile_dir: &Path,
        profile_home: Option<&Path>,
        timeout: Option<Duration>,
    ) -> Result<CapturedOutput, String> {
        let args = vec!["-p".to_string(), QUOTA_PROBE_PROMPT.to_string()];
        let mut cmd = build_agy_command(profile_dir, &args)?;
        cmd.current_dir(profile_dir);
        apply_profile_home(&mut cmd, profile_home);
        run_command_captured_interruptibly_with_timeout(&mut cmd, timeout)
    }
}

#[cfg(test)]
#[derive(Clone)]
pub struct MockProbeRunner {
    pub result: std::sync::Arc<std::sync::Mutex<Result<CapturedOutput, String>>>,
}

#[cfg(test)]
impl MockProbeRunner {
    pub fn new(result: Result<CapturedOutput, String>) -> Self {
        Self {
            result: std::sync::Arc::new(std::sync::Mutex::new(result)),
        }
    }
}

#[cfg(test)]
impl QuotaProbeRunner for MockProbeRunner {
    fn run_probe(
        &self,
        _profile_dir: &Path,
        _profile_home: Option<&Path>,
        _timeout: Option<Duration>,
    ) -> Result<CapturedOutput, String> {
        self.result.lock().unwrap().clone()
    }
}

/// Concurrent quota runs on different profiles share one cache file, so updates are merged
/// into a freshly loaded copy under a short dedicated lock instead of overwriting it.
fn merge_quota_cache_updates(
    cache_path: &Path,
    updates: Vec<CachedQuotaEntry>,
) -> Result<(), String> {
    let lock_path = cache_path.with_extension("lock");
    let _cache_lock =
        crate::security::FileLock::acquire_with_timeout(&lock_path, CACHE_LOCK_TIMEOUT)?;
    let mut cache = load_quota_cache(cache_path);
    for entry in updates {
        cache.entries.insert(entry.profile.clone(), entry);
    }
    save_quota_cache(cache_path, &cache)
}

fn build_error_output(profile: &str, email: Option<&str>, err_msg: &str) -> QuotaJsonOutput {
    let now = Utc::now().to_rfc3339();
    let result = serde_json::json!({
        "profile": profile,
        "email": email,
        "status": "error",
        "observed_at": now,
        "cached": false,
        "source": "preflight",
        "detail": err_msg,
        "agent_exit_code": null,
    });
    QuotaJsonOutput {
        schema_version: 1,
        checked_at: now,
        source: "preflight".to_string(),
        results: vec![result],
        cli_exit_code: 1,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_quota_impl<B: KeychainBackend + Clone, R: QuotaProbeRunner>(
    profile_arg: Option<&str>,
    all_arg: bool,
    timeout_str: Option<&str>,
    no_cache: bool,
    backend: B,
    runner: R,
) -> (Result<(), i32>, QuotaJsonOutput) {
    let start_instant = std::time::Instant::now();

    // 1. Argument validation: exactly one of --profile or --all is required
    if profile_arg.is_none() && !all_arg {
        let err_msg = "Exactly one of --profile or --all is required.";
        return (Err(1), build_error_output("unknown", None, err_msg));
    }
    if profile_arg.is_some() && all_arg {
        let target = profile_arg.unwrap_or("unknown");
        let err_msg = "Cannot specify both --profile and --all.";
        return (Err(1), build_error_output(target, None, err_msg));
    }

    // 2. Parse timeout
    let timeout_duration = match timeout_str {
        Some(s) => match parse_duration(s) {
            Ok(d) => d,
            Err(e) => {
                let target = profile_arg.unwrap_or("unknown");
                return (Err(1), build_error_output(target, None, &e));
            }
        },
        None => DEFAULT_QUOTA_TIMEOUT,
    };

    // 3. Load registry
    let config_path = ProfileRegistry::get_config_file_path();
    let reg = match ProfileRegistry::load_or_default(&config_path) {
        Ok(r) => r,
        Err(e) => {
            let target = profile_arg.unwrap_or("unknown");
            return (
                Err(1),
                build_error_output(target, None, &format!("Failed to load registry: {e}")),
            );
        }
    };

    // 4. Determine target profiles
    let target_profile_names: Vec<String> = if all_arg {
        let mut names: Vec<String> = reg.profiles.keys().cloned().collect();
        names.sort();
        names
    } else {
        let req_name = profile_arg.unwrap();
        match reg.resolve_profile(req_name) {
            Some(p) => vec![p.name.clone()],
            None => {
                let err_msg = format!("Profile or alias '{req_name}' not found.");
                return (Err(1), build_error_output(req_name, None, &err_msg));
            }
        }
    };

    if target_profile_names.is_empty() {
        return (
            Err(1),
            build_error_output("all", None, "No registered profiles found to check."),
        );
    }

    let elapsed = start_instant.elapsed();
    if elapsed >= timeout_duration {
        let target = target_profile_names
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown");
        return (
            Err(1),
            build_error_output(
                target,
                None,
                &format!("Timed out after {:?}", timeout_duration),
            ),
        );
    }
    // 5. Profiles are locked individually below, so other profiles keep running in parallel.

    // 6. Load cache
    let cache_path = ProfileRegistry::get_profiles_dir().join(QUOTA_CACHE_FILENAME);
    let cache = load_quota_cache(&cache_path);
    let mut cache_updates: Vec<CachedQuotaEntry> = Vec::new();

    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(target_profile_names.len());
    let mut statuses: Vec<QuotaStatus> = Vec::with_capacity(target_profile_names.len());

    let mut restoration_failed = false;

    for name in &target_profile_names {
        let profile = match reg.profiles.get(name) {
            Some(p) => p,
            None => continue,
        };

        let email_opt = if profile.email.is_empty() || profile.email == "unknown" {
            None
        } else {
            Some(profile.email.clone())
        };

        // Each profile lock re-arms the signal handlers, so an interrupt during an earlier
        // profile must stop the run here instead of being cleared by the next acquisition.
        if ProcessLock::interrupted() {
            results.push(serde_json::json!({
                "profile": profile.name,
                "email": email_opt,
                "status": "error",
                "observed_at": Utc::now().to_rfc3339(),
                "cached": false,
                "source": "preflight",
                "detail": "Interrupted.",
                "agent_exit_code": null,
            }));
            statuses.push(QuotaStatus::Error);
            break;
        }

        // Check if overall timeout expired before starting this profile
        if start_instant.elapsed() >= timeout_duration {
            let item = serde_json::json!({
                "profile": profile.name,
                "email": email_opt,
                "status": "error",
                "observed_at": Utc::now().to_rfc3339(),
                "cached": false,
                "source": "probe",
                "detail": format!("Timed out after {:?}", timeout_duration),
                "agent_exit_code": null,
            });
            results.push(item);
            statuses.push(QuotaStatus::Error);
            break;
        }

        // Check cache if --no-cache is not requested
        if !no_cache {
            if let Some(cached_entry) = cache.entries.get(&profile.name) {
                if let Ok(observed_dt) = DateTime::parse_from_rfc3339(&cached_entry.observed_at) {
                    let age = Utc::now().signed_duration_since(observed_dt.with_timezone(&Utc));
                    if age >= chrono::Duration::zero()
                        && age < chrono::Duration::from_std(QUOTA_CACHE_TTL).unwrap()
                    {
                        let status_enum = match cached_entry.status.as_str() {
                            "available" => Some(QuotaStatus::Available),
                            "quota_exhausted" => Some(QuotaStatus::QuotaExhausted),
                            "unauthenticated" => Some(QuotaStatus::Unauthenticated),
                            "unknown" => Some(QuotaStatus::Unknown),
                            "error" => Some(QuotaStatus::Error),
                            "unsupported" => Some(QuotaStatus::Unsupported),
                            _ => None,
                        };
                        if let Some(status_enum) = status_enum {
                            let item = serde_json::json!({
                                "profile": profile.name,
                                "email": email_opt,
                                "status": status_enum.as_str(),
                                "observed_at": cached_entry.observed_at,
                                "cached": true,
                                "source": "cache",
                                "detail": cached_entry.detail.as_deref().map(sanitize_detail),
                                "agent_exit_code": cached_entry.agent_exit_code,
                            });
                            results.push(item);
                            statuses.push(status_enum);
                            continue;
                        }
                    }
                }
            }
        }

        // Cache miss or --no-cache: Run fresh probe
        // Check stored auth
        let keychain_token_file = profile.dir.join("keychain_token.json");
        if !keychain_token_file.is_file() && !check_profile_auth(&profile.dir) {
            let item = serde_json::json!({
                "profile": profile.name,
                "email": email_opt,
                "status": "unauthenticated",
                "observed_at": Utc::now().to_rfc3339(),
                "cached": false,
                "source": "preflight",
                "detail": "Profile is not authenticated.",
                "agent_exit_code": null,
            });
            results.push(item);
            statuses.push(QuotaStatus::Unauthenticated);
            continue;
        }

        // Validate profile dir
        let validated_profile_dir = match validate_profile_dir_under(&profile.dir, &profiles_root) {
            Ok(d) => d,
            Err(e) => {
                let item = serde_json::json!({
                    "profile": profile.name,
                    "email": email_opt,
                    "status": "error",
                    "observed_at": Utc::now().to_rfc3339(),
                    "cached": false,
                    "source": "preflight",
                    "detail": sanitize_detail(&format!("Invalid profile directory: {e}")),
                    "agent_exit_code": null,
                });
                results.push(item);
                statuses.push(QuotaStatus::Error);
                continue;
            }
        };

        let transaction_deadline = start_instant + timeout_duration;
        let remaining_lock_timeout = transaction_deadline.saturating_duration_since(Instant::now());
        let lock_path = profile_lock_path(&ProfileRegistry::get_profiles_dir(), &profile.name);
        let profile_lock =
            match ProcessLock::acquire_with_timeout(&lock_path, remaining_lock_timeout) {
                Ok(l) => l,
                Err(e) => {
                    let timed_out = e.contains("Timed out");
                    let detail = if timed_out {
                        format!(
                            "Timed out waiting for process lock after {:?}",
                            timeout_duration
                        )
                    } else {
                        format!("Failed to acquire process lock: {e}")
                    };
                    results.push(serde_json::json!({
                        "profile": profile.name,
                        "email": email_opt,
                        "status": "error",
                        "observed_at": Utc::now().to_rfc3339(),
                        "cached": false,
                        "source": "preflight",
                        "detail": sanitize_detail(&detail),
                        "agent_exit_code": null,
                    }));
                    statuses.push(QuotaStatus::Error);
                    if timed_out {
                        break;
                    }
                    continue;
                }
            };

        // Snapshot the profile Keychain and activate its token under the command deadline.
        let (keychain_guard, profile_home) = match activate_profile(
            &validated_profile_dir,
            backend.clone(),
            transaction_deadline,
            profile_lock,
        ) {
            Ok(activated) => activated,
            Err(e) => {
                let item = serde_json::json!({
                    "profile": profile.name,
                    "email": email_opt,
                    "status": "error",
                    "observed_at": Utc::now().to_rfc3339(),
                    "cached": false,
                    "source": "preflight",
                    "detail": sanitize_detail(&format!("Failed to snapshot or activate Keychain: {e}")),
                    "agent_exit_code": null,
                });
                results.push(item);
                statuses.push(QuotaStatus::Error);
                continue;
            }
        };

        let elapsed_before_exec = start_instant.elapsed();
        let (output_res, timed_out_before_exec) = if elapsed_before_exec >= timeout_duration {
            (
                Ok(CapturedOutput {
                    status: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    timed_out: true,
                }),
                true,
            )
        } else {
            let remaining_exec = timeout_duration - elapsed_before_exec;
            (
                runner.run_probe(
                    &validated_profile_dir,
                    profile_home.as_deref(),
                    Some(remaining_exec),
                ),
                false,
            )
        };

        // Always restore Keychain. Once the overall deadline has elapsed, use a separately
        // bounded emergency cleanup window so the selected credential is not left active.
        let restore_deadline = if Instant::now() < transaction_deadline {
            transaction_deadline
        } else {
            Instant::now() + RESTORE_EMERGENCY_TIMEOUT
        };
        let restore_res = keychain_guard.sync_and_restore_until(restore_deadline);
        let now_str = Utc::now().to_rfc3339();

        if let Err(_restore_err) = restore_res {
            let item = serde_json::json!({
                "profile": profile.name,
                "email": email_opt,
                "status": "error",
                "observed_at": now_str,
                "cached": false,
                "source": "probe",
                "detail": "Keychain restoration failed.",
                "agent_exit_code": output_res.as_ref().ok().and_then(|o| o.status.and_then(|s| s.code())),
            });
            results.push(item);
            statuses.push(QuotaStatus::Error);
            restoration_failed = true;
            // Keychain safety requires immediate stop!
            break;
        }

        match output_res {
            Ok(output) => {
                let is_timed_out = output.timed_out || timed_out_before_exec;
                let exit_code = if is_timed_out {
                    None
                } else {
                    output.status.and_then(|s| s.code())
                };

                let stdout_str = String::from_utf8_lossy(&output.stdout);
                let stderr_str = String::from_utf8_lossy(&output.stderr);

                let (status, detail) =
                    classify_probe_outcome(is_timed_out, exit_code, &stdout_str, &stderr_str);

                let item = serde_json::json!({
                    "profile": profile.name,
                    "email": email_opt,
                    "status": status.as_str(),
                    "observed_at": now_str.clone(),
                    "cached": false,
                "source": "probe",
                    "detail": detail,
                    "agent_exit_code": exit_code,
                });
                results.push(item);
                statuses.push(status);

                // A total command deadline stops scheduling any later profile. Restoration has
                // already been attempted above, so breaking here preserves exact Keychain
                // transaction ordering without reporting profiles that were never probed.
                if is_timed_out || start_instant.elapsed() >= timeout_duration {
                    break;
                }

                // Update cache for definitive states
                if matches!(
                    status,
                    QuotaStatus::Available
                        | QuotaStatus::QuotaExhausted
                        | QuotaStatus::Unauthenticated
                        | QuotaStatus::Unknown
                ) {
                    cache_updates.push(CachedQuotaEntry {
                        profile: profile.name.clone(),
                        email: email_opt.clone(),
                        status: status.as_str().to_string(),
                        observed_at: now_str,
                        detail,
                        agent_exit_code: exit_code,
                    });
                }
            }
            Err(e) => {
                let is_unsupported = e.contains("Could not find 'agy' binary");
                let status = if is_unsupported {
                    QuotaStatus::Unsupported
                } else {
                    QuotaStatus::Error
                };
                let item = serde_json::json!({
                    "profile": profile.name,
                    "email": email_opt,
                    "status": status.as_str(),
                    "observed_at": now_str,
                    "cached": false,
                    "source": "preflight",
                    "detail": sanitize_detail(&e),
                    "agent_exit_code": null,
                });
                results.push(item);
                statuses.push(status);
            }
        }
    }

    if !cache_updates.is_empty() {
        if let Err(error) = merge_quota_cache_updates(&cache_path, cache_updates) {
            results.clear();
            statuses.clear();
            results.push(serde_json::json!({
                "profile": target_profile_names.first().map(String::as_str).unwrap_or("unknown"),
                "email": null,
                "status": "error",
                "observed_at": Utc::now().to_rfc3339(),
                "cached": false,
                "source": "probe",
                "detail": sanitize_detail(&format!("Quota cache operation failed: {error}")),
                "agent_exit_code": null,
            }));
            statuses.push(QuotaStatus::Error);
        }
    }

    let cli_exit_code = if restoration_failed {
        1
    } else {
        calculate_quota_exit_code(&statuses)
    };

    let checked_at = Utc::now().to_rfc3339();
    let has_cache = results
        .iter()
        .any(|item| item["source"].as_str() == Some("cache"));
    let has_probe = results
        .iter()
        .any(|item| item["source"].as_str() == Some("probe"));
    let has_preflight = results
        .iter()
        .any(|item| item["source"].as_str() == Some("preflight"));
    let source = match (has_cache, has_probe, has_preflight) {
        (true, false, false) => "cache",
        (false, true, false) => "probe",
        (false, false, true) => "preflight",
        _ => "mixed",
    };
    let json_doc = QuotaJsonOutput {
        schema_version: 1,
        checked_at,
        source: source.to_string(),
        results,
        cli_exit_code,
    };

    let res = if cli_exit_code == 0 {
        Ok(())
    } else {
        Err(cli_exit_code)
    };
    (res, json_doc)
}

pub fn print_quota_output(json_doc: &QuotaJsonOutput, json_mode: bool) {
    if json_mode {
        println!("{}", serde_json::to_string_pretty(json_doc).unwrap());
    } else {
        if json_doc.results.len() == 1 {
            let item = &json_doc.results[0];
            let status = item["status"].as_str().unwrap_or("");
            let detail = item["detail"].as_str().unwrap_or("");
            let source = item["source"].as_str().unwrap_or("");
            if status == "error" && source == "preflight" {
                eprintln!("Error: {detail}");
                return;
            }
        }

        println!(
            "{:<15} {:<30} {:<18} {:<8} DETAIL",
            "PROFILE", "EMAIL", "STATUS", "CACHED"
        );
        println!("{}", "-".repeat(90));
        for item in &json_doc.results {
            let profile = item["profile"].as_str().unwrap_or("-");
            let email = item["email"].as_str().unwrap_or("-");
            let status = item["status"].as_str().unwrap_or("-");
            let cached = if item["cached"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            };
            let detail = item["detail"].as_str().unwrap_or("");
            println!(
                "{:<15} {:<30} {:<18} {:<8} {}",
                profile, email, status, cached, detail
            );
        }
    }
}

pub fn cmd_quota(
    profile: Option<&str>,
    all: bool,
    timeout: Option<&str>,
    no_cache: bool,
    json: bool,
) -> Result<(), i32> {
    let (code, json_doc) = execute_quota_impl(
        profile,
        all,
        timeout,
        no_cache,
        SystemKeychainBackend,
        SystemProbeRunner,
    );
    print_quota_output(&json_doc, json);
    code
}

#[cfg(test)]
pub fn execute_quota_with_backend<B: KeychainBackend + Clone, R: QuotaProbeRunner>(
    profile: Option<&str>,
    all: bool,
    timeout: Option<&str>,
    no_cache: bool,
    backend: B,
    runner: R,
) -> (Result<(), i32>, QuotaJsonOutput) {
    execute_quota_impl(profile, all, timeout, no_cache, backend, runner)
}
