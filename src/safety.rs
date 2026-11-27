//! Account-safety guards for launching `agy`.
//!
//! Running several Google accounts from one machine is a grey area under the Antigravity terms,
//! and automated, bursty, parallel use is the pattern abuse detection looks for. These guards keep
//! agyp's traffic closer to what one person at one desk produces:
//!
//! - **Concurrency cap**: at most `AGYP_MAX_CONCURRENT` (default 4) agyp-launched `agy` sessions
//!   run at once across every profile. Extra launches wait for a free slot.
//! - **Launch pacing**: consecutive launches, on any profile, are spaced by `AGYP_LAUNCH_GAP_SECS`
//!   (default 15 s) plus a random `0..AGYP_LAUNCH_JITTER_SECS` (default 10 s), so a batch of tasks
//!   never hits Google as a same-second burst from several accounts.
//! - **Suspension stop**: when `agy` reports that the service was disabled for a Terms of Service
//!   violation, the profile is marked suspended and agyp refuses to run it until the user clears
//!   the mark with `agyp unsuspend <profile>` (after appealing). Every later launch on another
//!   profile prints a warning, because Google may link the accounts on this machine.
//!
//! All state lives under the profiles directory (`AGYP_PROFILES_DIR`, default
//! `~/.gemini-profiles`): `run-slots/`, `launch-pace.json` and `suspended/<profile>.json`.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::Utc;
use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub const DEFAULT_MAX_CONCURRENT: usize = 4;
pub const DEFAULT_LAUNCH_GAP_SECS: u64 = 15;
pub const DEFAULT_LAUNCH_JITTER_SECS: u64 = 10;
/// Exit code for a profile Google has suspended (distinct from `2` = unauthenticated).
pub const SUSPENDED_EXIT_CODE: i32 = 3;

const SLOT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const PACE_FILE: &str = "launch-pace.json";
const PACE_LOCK_FILE: &str = "launch-pace.lock";
const SLOTS_DIR: &str = "run-slots";
const SUSPENDED_DIR: &str = "suspended";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetySettings {
    /// `0` disables the cap.
    pub max_concurrent: usize,
    pub launch_gap: Duration,
    pub launch_jitter: Duration,
}

impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            max_concurrent: DEFAULT_MAX_CONCURRENT,
            launch_gap: Duration::from_secs(DEFAULT_LAUNCH_GAP_SECS),
            launch_jitter: Duration::from_secs(DEFAULT_LAUNCH_JITTER_SECS),
        }
    }
}

impl SafetySettings {
    pub fn from_env() -> Self {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Reads the settings through `lookup`; unset or malformed values keep the default.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let parse = |key: &str| lookup(key).and_then(|v| v.trim().parse::<u64>().ok());
        let defaults = Self::default();
        Self {
            max_concurrent: parse("AGYP_MAX_CONCURRENT")
                .map(|v| v as usize)
                .unwrap_or(defaults.max_concurrent),
            launch_gap: parse("AGYP_LAUNCH_GAP_SECS")
                .map(Duration::from_secs)
                .unwrap_or(defaults.launch_gap),
            launch_jitter: parse("AGYP_LAUNCH_JITTER_SECS")
                .map(Duration::from_secs)
                .unwrap_or(defaults.launch_jitter),
        }
    }
}

// ---------------------------------------------------------------------------
// Suspension detection and markers
// ---------------------------------------------------------------------------

/// True when `agy` output says Google disabled the service for a Terms of Service violation,
/// e.g. `403 ... This service has been disabled in this account for violation of Terms of
/// Service. Please submit an appeal to continue using this product.`
pub fn is_tos_suspension_text(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("violation of terms of service")
        || lower.contains("violation of the terms of service")
        || (lower.contains("service has been disabled") && lower.contains("appeal"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuspensionRecord {
    pub profile: String,
    pub detected_at: String,
    /// Which agyp command saw the message (`task`, `prompt`, `quota`, `usage`, `manual`).
    pub source: String,
    pub detail: Option<String>,
}

fn suspended_dir(profiles_dir: &Path) -> PathBuf {
    profiles_dir.join(SUSPENDED_DIR)
}

fn suspension_path(profiles_dir: &Path, profile: &str) -> Result<PathBuf, String> {
    crate::security::validate_component(profile, "profile")?;
    Ok(suspended_dir(profiles_dir).join(format!("{profile}.json")))
}

pub fn read_suspension(profiles_dir: &Path, profile: &str) -> Option<SuspensionRecord> {
    let path = suspension_path(profiles_dir, profile).ok()?;
    let meta = fs::symlink_metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok().or_else(|| {
        // A marker that cannot be parsed still means "suspended": fail closed.
        Some(SuspensionRecord {
            profile: profile.to_string(),
            detected_at: String::new(),
            source: "unknown".to_string(),
            detail: None,
        })
    })
}

pub fn list_suspensions(profiles_dir: &Path) -> Vec<SuspensionRecord> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(suspended_dir(profiles_dir)) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(profile) = name.strip_suffix(".json") {
            if let Some(record) = read_suspension(profiles_dir, profile) {
                out.push(record);
            }
        }
    }
    out.sort_by(|a, b| a.profile.cmp(&b.profile));
    out
}

pub fn mark_suspended(
    profiles_dir: &Path,
    profile: &str,
    source: &str,
    detail: Option<&str>,
) -> Result<SuspensionRecord, String> {
    let path = suspension_path(profiles_dir, profile)?;
    let dir = suspended_dir(profiles_dir);
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    crate::security::secure_dir(&dir)
        .map_err(|e| format!("Failed to secure {}: {e}", dir.display()))?;
    let record = SuspensionRecord {
        profile: profile.to_string(),
        detected_at: Utc::now().to_rfc3339(),
        source: source.to_string(),
        detail: detail.map(|d| d.chars().take(500).collect()),
    };
    let body = serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?;
    crate::security::write_private_file(&path, body.as_bytes())
        .map_err(|e| format!("Failed to write {}: {e}", path.display()))?;
    Ok(record)
}

/// Returns `Ok(true)` when a marker was removed.
pub fn clear_suspension(profiles_dir: &Path, profile: &str) -> Result<bool, String> {
    let path = suspension_path(profiles_dir, profile)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("Failed to remove {}: {e}", path.display())),
    }
}

/// Marks `profile` suspended when `text` carries the ToS message, and tells the user what to do.
/// Returns true when the profile was (or already is) suspended.
pub fn record_if_suspended(profiles_dir: &Path, profile: &str, source: &str, text: &str) -> bool {
    if !is_tos_suspension_text(text) {
        return false;
    }
    let detail = text
        .lines()
        .find(|line| is_tos_suspension_text(line))
        .map(str::trim);
    match mark_suspended(profiles_dir, profile, source, detail) {
        Ok(_) => eprintln!("{}", suspension_notice(profile)),
        Err(e) => eprintln!(
            "agyp: profile '{profile}' looks suspended by Google, but the marker could not be saved: {e}"
        ),
    }
    true
}

pub fn suspension_notice(profile: &str) -> String {
    format!(
        "agyp: Google disabled Antigravity for profile '{profile}' (Terms of Service). \
agyp will not run it again until you appeal and then run `agyp unsuspend {profile}`. \
Pause AGY work on the other profiles too: Google may link accounts used from this machine."
    )
}

// ---------------------------------------------------------------------------
// Launch pacing and concurrency
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum LaunchError {
    Suspended(SuspensionRecord),
    Io(String),
}

impl LaunchError {
    pub fn exit_code(&self) -> i32 {
        match self {
            LaunchError::Suspended(_) => SUSPENDED_EXIT_CODE,
            LaunchError::Io(_) => 1,
        }
    }

    pub fn message(&self) -> String {
        match self {
            LaunchError::Suspended(r) => format!(
                "Profile '{}' is marked suspended by Google (detected {} via {}). \
Appeal first, then run `agyp unsuspend {}`.",
                r.profile,
                if r.detected_at.is_empty() {
                    "earlier"
                } else {
                    &r.detected_at
                },
                r.source,
                r.profile
            ),
            LaunchError::Io(e) => format!("Launch guard failed: {e}"),
        }
    }
}

/// Holds one concurrency slot until dropped (the OS releases the lock if agyp dies).
#[derive(Debug)]
pub struct LaunchPermit {
    _slot: Option<File>,
}

/// How long to wait before the next launch, given the previous launch time.
pub fn compute_launch_wait(
    last_launch_ms: Option<u64>,
    now_ms: u64,
    gap: Duration,
    jitter: Duration,
) -> Duration {
    let Some(last) = last_launch_ms else {
        return Duration::ZERO;
    };
    let target = Duration::from_millis(last) + gap + jitter;
    target.saturating_sub(Duration::from_millis(now_ms))
}

/// Uniform-ish value in `0..=max`, without pulling in a RNG crate.
pub fn pseudo_random_jitter(max: Duration) -> Duration {
    let max_ms = max.as_millis() as u64;
    if max_ms == 0 {
        return Duration::ZERO;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos ^ ((std::process::id() as u64) << 32) ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    Duration::from_millis(x % (max_ms + 1))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PaceState {
    last_launch_ms: Option<u64>,
}

fn open_lock_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("Failed to open {}: {e}", path.display()))
}

fn acquire_slot(profiles_dir: &Path, max: usize, profile: &str) -> Result<Option<File>, String> {
    if max == 0 {
        return Ok(None);
    }
    let dir = profiles_dir.join(SLOTS_DIR);
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create {}: {e}", dir.display()))?;
    let mut announced = false;
    loop {
        for index in 0..max {
            let file = open_lock_file(&dir.join(format!("slot-{index}.lock")))?;
            if file.try_lock_exclusive().is_ok() {
                return Ok(Some(file));
            }
        }
        if !announced {
            eprintln!(
                "agyp: {max} AGY sessions are already running (AGYP_MAX_CONCURRENT={max}); \
profile '{profile}' waits for one to finish..."
            );
            announced = true;
        }
        std::thread::sleep(SLOT_POLL_INTERVAL);
    }
}

fn pace_launch(
    profiles_dir: &Path,
    settings: &SafetySettings,
    profile: &str,
) -> Result<(), String> {
    if settings.launch_gap.is_zero() && settings.launch_jitter.is_zero() {
        return Ok(());
    }
    fs::create_dir_all(profiles_dir)
        .map_err(|e| format!("Failed to create {}: {e}", profiles_dir.display()))?;
    let lock = open_lock_file(&profiles_dir.join(PACE_LOCK_FILE))?;
    // Held while sleeping, so queued launches leave one by one, each a full gap apart.
    lock.lock_exclusive()
        .map_err(|e| format!("Failed to lock launch pacing: {e}"))?;

    let state_path = profiles_dir.join(PACE_FILE);
    let state: PaceState = fs::read_to_string(&state_path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    let jitter = pseudo_random_jitter(settings.launch_jitter);
    let wait = compute_launch_wait(state.last_launch_ms, now_ms(), settings.launch_gap, jitter);
    if wait >= Duration::from_secs(1) {
        eprintln!(
            "agyp: pacing AGY launches, profile '{profile}' starts in {}s...",
            wait.as_secs()
        );
    }
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }

    let body = serde_json::to_string(&PaceState {
        last_launch_ms: Some(now_ms()),
    })
    .map_err(|e| e.to_string())?;
    let tmp = profiles_dir.join(format!("{PACE_FILE}.tmp"));
    {
        let mut f = File::create(&tmp).map_err(|e| format!("Failed to write pace state: {e}"))?;
        f.write_all(body.as_bytes())
            .map_err(|e| format!("Failed to write pace state: {e}"))?;
    }
    fs::rename(&tmp, &state_path).map_err(|e| format!("Failed to save pace state: {e}"))?;
    let _ = FileExt::unlock(&lock);
    Ok(())
}

fn warn_about_other_suspensions(profiles_dir: &Path, profile: &str) {
    let others: Vec<String> = list_suspensions(profiles_dir)
        .into_iter()
        .filter(|r| r.profile != profile)
        .map(|r| r.profile)
        .collect();
    if !others.is_empty() {
        eprintln!(
            "agyp: warning: Google suspended profile(s) {} on this machine. \
Running more accounts from here can put them at risk too.",
            others.join(", ")
        );
    }
}

/// Gate every model-using `agy` launch through this: refuses a suspended profile, then waits for
/// a concurrency slot and for the launch pacing window. Keep the permit alive for the whole run.
pub fn acquire_launch_permit(
    profiles_dir: &Path,
    profile: &str,
    settings: &SafetySettings,
) -> Result<LaunchPermit, LaunchError> {
    if let Some(record) = read_suspension(profiles_dir, profile) {
        return Err(LaunchError::Suspended(record));
    }
    warn_about_other_suspensions(profiles_dir, profile);
    let slot =
        acquire_slot(profiles_dir, settings.max_concurrent, profile).map_err(LaunchError::Io)?;
    pace_launch(profiles_dir, settings, profile).map_err(LaunchError::Io)?;
    Ok(LaunchPermit { _slot: slot })
}

/// `acquire_launch_permit` with the real profiles directory and env settings, printing the error.
pub fn guard_launch(profile: &str) -> Result<LaunchPermit, i32> {
    let profiles_dir = crate::config::ProfileRegistry::get_profiles_dir();
    acquire_launch_permit(&profiles_dir, profile, &SafetySettings::from_env()).map_err(|e| {
        eprintln!("Error: {}", e.message());
        e.exit_code()
    })
}
