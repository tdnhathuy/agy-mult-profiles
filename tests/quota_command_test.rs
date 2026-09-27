use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
#[allow(dead_code)]
mod commands;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/keychain.rs"]
mod keychain;
#[path = "../src/security.rs"]
mod security;

use config::{ProfileInfo, ProfileRegistry};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_test_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvVarGuard {
    key: &'static str,
    orig: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
        let orig = std::env::var_os(key);
        std::env::set_var(key, val);
        Self { key, orig }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        if let Some(ref orig) = self.orig {
            std::env::set_var(self.key, orig);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

fn setup_test_bin_dir(dir: &Path, mock_keychain_file: &Path, agy_script_content: &str) -> PathBuf {
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();

    let agy_path = bin_dir.join("agy");
    let mut file = fs::File::create(&agy_path).unwrap();
    file.write_all(agy_script_content.as_bytes()).unwrap();
    let mut perms = file.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&agy_path, perms).unwrap();

    let sec_path = bin_dir.join("security");
    let sec_script = format!(
        r#"#!/bin/sh
KEYCHAIN_FILE="{}"
cmd="$1"
shift
case "$cmd" in
  find-generic-password)
    if [ -f "$KEYCHAIN_FILE" ]; then
      cat "$KEYCHAIN_FILE"
      exit 0
    else
      echo "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain." >&2
      exit 44
    fi
    ;;
  add-generic-password)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "-w" ]; then
        printf "%s" "$2" > "$KEYCHAIN_FILE"
        exit 0
      fi
      shift
    done
    exit 0
    ;;
  delete-generic-password)
    rm -f "$KEYCHAIN_FILE"
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#,
        mock_keychain_file.display()
    );
    let mut f2 = fs::File::create(&sec_path).unwrap();
    f2.write_all(sec_script.as_bytes()).unwrap();
    let mut perms2 = f2.metadata().unwrap().permissions();
    perms2.set_mode(0o755);
    fs::set_permissions(&sec_path, perms2).unwrap();

    bin_dir
}

fn setup_test_profile(profiles_dir: &Path, name: &str, email: &str) -> (PathBuf, PathBuf) {
    let profile_dir = profiles_dir.join("profiles").join(name);
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(
        profile_dir.join("keychain_token.json"),
        r#"{"token":{"id_token":"dummy"},"email":"test@example.com"}"#,
    )
    .unwrap();

    let config_file = profiles_dir.join("profiles.json");
    let mut reg = ProfileRegistry::load_or_default(&config_file).unwrap();
    reg.add_profile(ProfileInfo {
        name: name.to_string(),
        email: email.to_string(),
        dir: profile_dir.clone(),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.save(&config_file).unwrap();

    let mock_keychain_file = profiles_dir.join("mock_system_keychain.json");
    fs::write(&mock_keychain_file, "initial-system-token").unwrap();

    (profile_dir, mock_keychain_file)
}

// -------------------------------------------------------------
// 1. CLI Exclusivity, Help, and JSON Error Output
// -------------------------------------------------------------

#[test]
fn test_cli_quota_missing_profile_value_json_schema() {
    let _lock = lock_test_env();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agyp"))
        .args(["quota", "--profile", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["source"], "preflight");
    assert_eq!(value["cli_exit_code"], 1);
    assert_eq!(value["results"][0]["profile"], "unknown");
    assert_eq!(value["results"][0]["status"], "error");
    assert_eq!(value["results"][0]["source"], "preflight");
    assert_eq!(value["results"][0]["cached"], false);
    assert!(value["results"][0]["observed_at"].is_string());
    assert!(value["results"][0]["detail"].is_string());
    assert!(value["results"][0]["agent_exit_code"].is_null());
}

#[test]
fn test_sanitize_detail_preserves_known_probe_outcomes() {
    assert_eq!(
        commands::quota::sanitize_detail("Probe completed successfully"),
        "Probe completed successfully"
    );
    assert_eq!(
        commands::quota::sanitize_detail("Probe completed without expected output"),
        "Probe completed without expected output"
    );
}

#[test]
fn test_cli_quota_exclusivity_missing_args() {
    let bin = env!("CARGO_BIN_EXE_agyp");

    // 1. Missing both --profile and --all without --json -> exit code 1
    let out = Command::new(bin).args(["quota"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Exactly one of --profile or --all is required"),
        "Unexpected stderr: {stderr}"
    );

    // 2. Missing both with --json -> exit code 1 and valid single JSON document
    let out_json = Command::new(bin)
        .args(["quota", "--json"])
        .output()
        .unwrap();
    assert_eq!(out_json.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out_json.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse JSON: {e}\nstdout was:\n{stdout}"));
    assert_eq!(val["schema_version"], 1);
    assert_eq!(val["cli_exit_code"], 1);
    assert_eq!(val["source"], "preflight");
    assert_eq!(val["results"].as_array().unwrap().len(), 1);
    assert_eq!(val["results"][0]["status"], "error");
    assert_eq!(val["results"][0]["source"], "preflight");
    assert_eq!(val["results"][0]["profile"], "unknown");
    assert!(val["results"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("Exactly one of --profile or --all is required"));
}

#[test]
fn test_cli_quota_exclusivity_both_args() {
    let bin = env!("CARGO_BIN_EXE_agyp");

    // 1. Both --profile and --all without --json -> exit code 1
    let out = Command::new(bin)
        .args(["quota", "--profile", "dev", "--all"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("Cannot specify both --profile and --all"),
        "Unexpected stderr: {stderr}"
    );

    // 2. Both with --json -> exit code 1 and valid JSON
    let out_json = Command::new(bin)
        .args(["quota", "--profile", "dev", "--all", "--json"])
        .output()
        .unwrap();
    assert_eq!(out_json.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out_json.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("Failed to parse JSON: {e}\nstdout was:\n{stdout}"));
    assert_eq!(val["schema_version"], 1);
    assert_eq!(val["cli_exit_code"], 1);
    assert_eq!(val["source"], "preflight");
    assert_eq!(val["results"].as_array().unwrap().len(), 1);
    assert_eq!(val["results"][0]["status"], "error");
    assert_eq!(val["results"][0]["source"], "preflight");
    assert_eq!(val["results"][0]["profile"], "dev");
    assert!(val["results"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("Cannot specify both --profile and --all"));
}

#[test]
fn test_cli_quota_invalid_profile() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();

    let out = Command::new(bin)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "nonexistent", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["schema_version"], 1);
    assert_eq!(val["cli_exit_code"], 1);
    assert_eq!(val["source"], "preflight");
    assert_eq!(val["results"][0]["profile"], "nonexistent");
    assert_eq!(val["results"][0]["status"], "error");
    assert_eq!(val["results"][0]["source"], "preflight");
    assert!(val["results"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("Profile or alias 'nonexistent' not found"));
}

#[test]
fn test_cli_quota_all_empty_registry() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();

    // 1. JSON mode: empty registry must exit 1 with structured non-success result
    let out_json = Command::new(bin)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--all", "--json"])
        .output()
        .unwrap();
    assert_eq!(out_json.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out_json.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["schema_version"], 1);
    assert_eq!(val["cli_exit_code"], 1);
    assert_eq!(val["source"], "preflight");
    let results = val["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["status"], "error");
    assert_eq!(results[0]["source"], "preflight");
    assert!(results[0]["detail"]
        .as_str()
        .unwrap()
        .contains("No registered profiles found"));

    // 2. Plain text mode: empty registry must exit 1 and write to stderr
    let out_plain = Command::new(bin)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--all"])
        .output()
        .unwrap();
    assert_eq!(out_plain.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out_plain.stderr);
    assert!(stderr.contains("No registered profiles found"));
}

#[test]
fn test_cli_quota_help() {
    let bin = env!("CARGO_BIN_EXE_agyp");
    let out = Command::new(bin)
        .args(["quota", "--help"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("--profile"));
    assert!(stdout.contains("--all"));
    assert!(stdout.contains("--timeout"));
    assert!(stdout.contains("--no-cache"));
    assert!(stdout.contains("--json"));
}

// -------------------------------------------------------------
// 2. Stable Ordering for --all & Alias Resolution
// -------------------------------------------------------------

#[test]
fn test_quota_stable_ordering_for_all() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();

    // Create 3 profiles in deliberately unsorted order: zebra, alpha, beta
    setup_test_profile(profiles_temp.path(), "zebra", "zebra@example.com");
    setup_test_profile(profiles_temp.path(), "alpha", "alpha@example.com");
    let (_pdir, mock_keychain) =
        setup_test_profile(profiles_temp.path(), "beta", "beta@example.com");

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--all", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let results = val["results"].as_array().unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0]["profile"], "alpha");
    assert_eq!(results[1]["profile"], "beta");
    assert_eq!(results[2]["profile"], "zebra");
}

#[test]
fn test_quota_alias_resolution() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(
        profiles_temp.path(),
        "canonical-profile",
        "user@example.com",
    );

    // Add alias
    let config_file = profiles_temp.path().join("profiles.json");
    let mut reg = ProfileRegistry::load_or_default(&config_file).unwrap();
    reg.set_alias("p1", "canonical-profile").unwrap();
    reg.save(&config_file).unwrap();

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "p1", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["cli_exit_code"], 0);
    assert_eq!(val["results"][0]["profile"], "canonical-profile");
    assert_eq!(val["results"][0]["status"], "available");
}

// -------------------------------------------------------------
// 3. Exact Classifier Cases & False-Positive Prose Regressions
// -------------------------------------------------------------

#[test]
fn test_quota_classifier_exact_cases() {
    use commands::quota::{classify_probe_outcome, QuotaStatus};

    // 1. Available: exit 0 + stdout has PONG
    let (status, detail) = classify_probe_outcome(false, Some(0), "PONG\n", "");
    assert_eq!(status, QuotaStatus::Available);
    assert!(detail.is_some());

    // 2. Available: case-insensitive pong
    let (status, _) = classify_probe_outcome(false, Some(0), "pong", "");
    assert_eq!(status, QuotaStatus::Available);

    // 3. Unknown: exit 0 but unexpected output
    let (status, detail) = classify_probe_outcome(false, Some(0), "Hello world\n", "");
    assert_eq!(status, QuotaStatus::Unknown);
    assert!(detail.unwrap().contains("without expected output"));

    // 4. Unknown: exit 0 but empty stdout
    let (status, _) = classify_probe_outcome(false, Some(0), "", "");
    assert_eq!(status, QuotaStatus::Unknown);

    // 5. Quota Exhausted: anchored 429 in stderr
    let (status, _) = classify_probe_outcome(false, Some(1), "", "Error: 429 RESOURCE_EXHAUSTED\n");
    assert_eq!(status, QuotaStatus::QuotaExhausted);

    let (status, _) = classify_probe_outcome(false, Some(1), "", "429: resource_exhausted\n");
    assert_eq!(status, QuotaStatus::QuotaExhausted);

    let (status, _) =
        classify_probe_outcome(false, Some(1), "", "RESOURCE_EXHAUSTED: rate limit\n");
    assert_eq!(status, QuotaStatus::QuotaExhausted);

    let (status, _) =
        classify_probe_outcome(false, Some(1), "", "Individual quota reached for today\n");
    assert_eq!(status, QuotaStatus::QuotaExhausted);

    let (status, _) =
        classify_probe_outcome(false, Some(1), "", "Quota exceeded for quota metric\n");
    assert_eq!(status, QuotaStatus::QuotaExhausted);

    // 6. Unauthenticated: anchored auth error in stderr
    let (status, _) = classify_probe_outcome(false, Some(1), "", "Error: unauthenticated\n");
    assert_eq!(status, QuotaStatus::Unauthenticated);

    let (status, _) = classify_probe_outcome(false, Some(1), "", "oauth2: \"invalid_grant\"\n");
    assert_eq!(status, QuotaStatus::Unauthenticated);

    // 7. Unknown: exit 1 with unrecognized error text
    let (status, _) = classify_probe_outcome(false, Some(1), "", "random unhandled error\n");
    assert_eq!(status, QuotaStatus::Unknown);

    // 8. Error: timeout
    let (status, _) = classify_probe_outcome(true, None, "", "");
    assert_eq!(status, QuotaStatus::Error);
}

#[test]
fn test_quota_strict_sentinel_classification() {
    use commands::quota::{classify_probe_outcome, QuotaStatus};

    // Positive cases: exact PONG with harmless surrounding whitespace
    assert_eq!(
        classify_probe_outcome(false, Some(0), "PONG", "").0,
        QuotaStatus::Available
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "PONG\n", "").0,
        QuotaStatus::Available
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "pong\n", "").0,
        QuotaStatus::Available
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "  PONG  \r\n", "").0,
        QuotaStatus::Available
    );

    // Negative cases: NOT PONG, PONG failed, PONGER, multiline prose, contradictory output
    assert_eq!(
        classify_probe_outcome(false, Some(0), "NOT PONG", "").0,
        QuotaStatus::Unknown
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "PONG failed", "").0,
        QuotaStatus::Unknown
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "PONGER", "").0,
        QuotaStatus::Unknown
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "PONG\nExtra trailing line", "").0,
        QuotaStatus::Unknown
    );
    assert_eq!(
        classify_probe_outcome(false, Some(0), "First line\nPONG", "").0,
        QuotaStatus::Unknown
    );
    // Contradictory output: exit 0 and PONG stdout but stderr has error output
    assert_eq!(
        classify_probe_outcome(false, Some(0), "PONG\n", "error: something failed").0,
        QuotaStatus::Unknown
    );
}

#[test]
fn test_quota_classifier_prose_false_positive_regressions() {
    use commands::quota::{classify_probe_outcome, QuotaStatus};

    // Multiline prose containing PONG and RESOURCE_EXHAUSTED must be Unknown (never Available, never QuotaExhausted)
    let (status, _) = classify_probe_outcome(
        false,
        Some(0),
        "PONG\nDiscussing RESOURCE_EXHAUSTED and 429 error handling in detail",
        "",
    );
    assert_eq!(
        status,
        QuotaStatus::Unknown,
        "Multiline prose containing PONG must be classified as Unknown"
    );

    // Exit 0 with the sentinel plus any stderr content is contradictory and must remain Unknown,
    // even when stderr only discusses an unanchored 429 in prose.
    let (status, _) = classify_probe_outcome(
        false,
        Some(0),
        "PONG\n",
        "Added test for error: 429 response handling\n",
    );
    assert_eq!(
        status,
        QuotaStatus::Unknown,
        "Any stderr output makes an otherwise successful sentinel contradictory"
    );

    // The same strict rule applies to prose mentioning unauthenticated paths.
    let (status, _) = classify_probe_outcome(
        false,
        Some(0),
        "PONG\n",
        "Tested unauthenticated retry paths\n",
    );
    assert_eq!(
        status,
        QuotaStatus::Unknown,
        "Any stderr output makes an otherwise successful sentinel contradictory"
    );
}

// -------------------------------------------------------------
// 4. Timeout and No Retry
// -------------------------------------------------------------

#[test]
fn test_quota_timeout_and_no_retry() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let marker_file = profiles_temp.path().join("probe_run_count.txt");

    // Script that records run count and hangs
    let script = format!(
        r#"#!/bin/sh
count=0
if [ -f "{}" ]; then
  count=$(cat "{}")
fi
count=$((count + 1))
printf "%s" "$count" > "{}"
sleep 10
echo "PONG"
exit 0
"#,
        marker_file.display(),
        marker_file.display(),
        marker_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, &script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--timeout", "1500ms", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["results"][0]["status"], "error");
    assert!(val["results"][0]["detail"]
        .as_str()
        .unwrap()
        .contains("timed out"));

    // Verify probe ran exactly ONCE (never retried!)
    assert!(marker_file.exists());
    let run_count = fs::read_to_string(&marker_file).unwrap();
    assert_eq!(run_count, "1", "Probe must never be retried!");
}

// -------------------------------------------------------------
// 5. Cache Lifecycle: Fresh, Cached, --no-cache, TTL, 0600, Privacy, Corrupt
// -------------------------------------------------------------

#[test]
fn test_quota_cache_lifecycle() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let counter_file = profiles_temp.path().join("probe_execs.txt");
    let script = format!(
        r#"#!/bin/sh
c=0
if [ -f "{}" ]; then
  c=$(cat "{}")
fi
c=$((c + 1))
printf "%s" "$c" > "{}"
echo "PONG"
exit 0
"#,
        counter_file.display(),
        counter_file.display(),
        counter_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, &script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    // 1. First probe: fresh execution -> cached = false
    let out1 = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();
    assert_eq!(out1.status.code(), Some(0));
    let val1: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out1.stdout)).unwrap();
    assert_eq!(val1["results"][0]["status"], "available");
    assert_eq!(val1["results"][0]["cached"], false);
    assert_eq!(fs::read_to_string(&counter_file).unwrap(), "1");

    // Verify cache file exists, mode 0600, contains no raw body/token
    let cache_file = profiles_temp.path().join("quota-cache.json");
    assert!(cache_file.is_file(), "quota-cache.json must exist");
    let meta = fs::metadata(&cache_file).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "Cache file must have mode 0600, got {:o}",
        mode
    );

    let cache_content = fs::read_to_string(&cache_file).unwrap();
    assert!(
        !cache_content.contains("PONG"),
        "Cache must not store raw probe output"
    );
    assert!(
        !cache_content.contains("dummy"),
        "Cache must not store token credentials"
    );

    // 2. Second probe immediately: should hit cache -> cached = true, counter remains 1
    let out2 = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(0));
    let val2: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out2.stdout)).unwrap();
    assert_eq!(val2["results"][0]["status"], "available");
    assert_eq!(val2["results"][0]["cached"], true);
    assert_eq!(
        fs::read_to_string(&counter_file).unwrap(),
        "1",
        "Cached probe must not run executable"
    );

    // 3. Third probe with --no-cache: forces fresh probe -> cached = false, counter becomes 2
    let out3 = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--no-cache", "--json"])
        .output()
        .unwrap();
    assert_eq!(out3.status.code(), Some(0));
    let val3: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out3.stdout)).unwrap();
    assert_eq!(val3["results"][0]["status"], "available");
    assert_eq!(val3["results"][0]["cached"], false);
    assert_eq!(
        fs::read_to_string(&counter_file).unwrap(),
        "2",
        "--no-cache must run executable"
    );

    // 4. Test TTL expiration (5 minutes): set observed_at to 6 minutes ago
    let past_time = (chrono::Utc::now() - chrono::Duration::minutes(6)).to_rfc3339();
    let mut cache_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&cache_file).unwrap()).unwrap();
    cache_json["entries"]["dev"]["observed_at"] = serde_json::Value::String(past_time);
    fs::write(
        &cache_file,
        serde_json::to_string_pretty(&cache_json).unwrap(),
    )
    .unwrap();

    let out4 = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();
    assert_eq!(out4.status.code(), Some(0));
    let val4: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out4.stdout)).unwrap();
    assert_eq!(val4["results"][0]["status"], "available");
    assert_eq!(
        val4["results"][0]["cached"], false,
        "Expired TTL must force fresh probe"
    );
    assert_eq!(fs::read_to_string(&counter_file).unwrap(), "3");

    // 5. Test Corrupt Cache: write malformed JSON -> must safely degrade without leaking content
    fs::write(&cache_file, "INVALID_CORRUPT_JSON_DATA {{{").unwrap();
    let out5 = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();
    assert_eq!(out5.status.code(), Some(0));
    let val5: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out5.stdout)).unwrap();
    assert_eq!(val5["results"][0]["status"], "available");
    assert_eq!(val5["results"][0]["cached"], false);
    assert_eq!(fs::read_to_string(&counter_file).unwrap(), "4");
    // Verify corrupt string was not printed
    assert!(!String::from_utf8_lossy(&out5.stdout).contains("INVALID_CORRUPT_JSON_DATA"));
}

#[test]
fn test_quota_cache_symlink_rejected() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let target_file = profiles_temp.path().join("real_target.txt");
    fs::write(&target_file, "secret").unwrap();

    let cache_file = profiles_temp.path().join("quota-cache.json");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target_file, &cache_file).unwrap();

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["results"][0]["status"], "error");
    assert_eq!(
        val["results"][0]["detail"].as_str().unwrap(),
        "Quota cache operation failed."
    );
}

#[test]
fn test_quota_cache_unsafe_permissions_repaired() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    // Seed cache with 0666 permissions
    let cache_file = profiles_temp.path().join("quota-cache.json");
    let valid_cache = serde_json::json!({
        "version": 1,
        "entries": {
            "dev": {
                "profile": "dev",
                "email": "dev@example.com",
                "status": "available",
                "observed_at": chrono::Utc::now().to_rfc3339(),
                "detail": "Probe completed successfully",
                "agent_exit_code": 0
            }
        }
    });
    fs::write(&cache_file, serde_json::to_string(&valid_cache).unwrap()).unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&cache_file).unwrap().permissions();
        perms.set_mode(0o666);
        fs::set_permissions(&cache_file, perms).unwrap();
    }

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(val["results"][0]["status"], "available");

    // Must be repaired to 0600
    #[cfg(unix)]
    {
        let mode = fs::metadata(&cache_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "Cache permissions must be repaired to 0600");
    }
}

#[test]
fn test_quota_cache_unknown_status_treated_as_miss() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let counter_file = profiles_temp.path().join("probe_execs.txt");
    let script = format!(
        r#"#!/bin/sh
c=0
if [ -f "{}" ]; then
  c=$(cat "{}")
fi
c=$((c + 1))
printf "%s" "$c" > "{}"
echo "PONG"
exit 0
"#,
        counter_file.display(),
        counter_file.display(),
        counter_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, &script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let cache_file = profiles_temp.path().join("quota-cache.json");
    let bogus_cache = serde_json::json!({
        "version": 1,
        "entries": {
            "dev": {
                "profile": "dev",
                "email": "dev@example.com",
                "status": "some_bogus_status",
                "observed_at": chrono::Utc::now().to_rfc3339(),
                "detail": "Bogus detail",
                "agent_exit_code": 0
            }
        }
    });
    fs::write(&cache_file, serde_json::to_string(&bogus_cache).unwrap()).unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&cache_file).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&cache_file, perms).unwrap();
    }

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(val["results"][0]["status"], "available");
    assert_eq!(val["results"][0]["cached"], false);
    assert_eq!(val["results"][0]["source"], "probe");
    assert_eq!(fs::read_to_string(&counter_file).unwrap(), "1");
}

#[test]
fn test_quota_cache_stale_identity_not_trusted() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) =
        setup_test_profile(profiles_temp.path(), "dev", "canonical@example.com");

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    // Cache with spoofed profile name, spoofed email, and malicious detail string
    let cache_file = profiles_temp.path().join("quota-cache.json");
    let spoofed_cache = serde_json::json!({
        "version": 1,
        "entries": {
            "dev": {
                "profile": "spoofed_hacker_profile",
                "email": "hacker@evil.com",
                "status": "available",
                "observed_at": chrono::Utc::now().to_rfc3339(),
                "detail": "MALICIOUS_INJECTED_DETAIL_PAYLOAD",
                "agent_exit_code": 0
            }
        }
    });
    fs::write(&cache_file, serde_json::to_string(&spoofed_cache).unwrap()).unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&cache_file).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&cache_file, perms).unwrap();
    }

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    // Must emit canonical registry profile and email
    assert_eq!(val["results"][0]["profile"], "dev");
    assert_eq!(val["results"][0]["email"], "canonical@example.com");
    assert_eq!(val["results"][0]["status"], "available");
    assert_eq!(val["results"][0]["cached"], true);
    assert_eq!(val["results"][0]["source"], "cache");
    // Must NEVER emit spoofed identity or arbitrary cached detail
    assert!(!stdout.contains("spoofed_hacker_profile"));
    assert!(!stdout.contains("hacker@evil.com"));
    assert!(!stdout.contains("MALICIOUS_INJECTED_DETAIL_PAYLOAD"));
}

#[test]
fn test_quota_cache_future_timestamp_treated_as_miss() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let counter_file = profiles_temp.path().join("probe_execs.txt");
    let script = format!(
        r#"#!/bin/sh
c=0
if [ -f "{}" ]; then
  c=$(cat "{}")
fi
c=$((c + 1))
printf "%s" "$c" > "{}"
echo "PONG"
exit 0
"#,
        counter_file.display(),
        counter_file.display(),
        counter_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, &script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let future_time = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let cache_file = profiles_temp.path().join("quota-cache.json");
    let future_cache = serde_json::json!({
        "version": 1,
        "entries": {
            "dev": {
                "profile": "dev",
                "email": "dev@example.com",
                "status": "available",
                "observed_at": future_time,
                "detail": "Probe completed successfully",
                "agent_exit_code": 0
            }
        }
    });
    fs::write(&cache_file, serde_json::to_string(&future_cache).unwrap()).unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&cache_file).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&cache_file, perms).unwrap();
    }

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(val["results"][0]["cached"], false);
    assert_eq!(val["results"][0]["source"], "probe");
    assert_eq!(fs::read_to_string(&counter_file).unwrap(), "1");
}

#[test]
fn test_quota_cache_missing_fields_treated_as_miss() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let counter_file = profiles_temp.path().join("probe_execs.txt");
    let script = format!(
        r#"#!/bin/sh
c=0
if [ -f "{}" ]; then
  c=$(cat "{}")
fi
c=$((c + 1))
printf "%s" "$c" > "{}"
echo "PONG"
exit 0
"#,
        counter_file.display(),
        counter_file.display(),
        counter_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, &script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let cache_file = profiles_temp.path().join("quota-cache.json");
    // Missing "status"
    let missing_cache = serde_json::json!({
        "version": 1,
        "entries": {
            "dev": {
                "profile": "dev",
                "email": "dev@example.com",
                "observed_at": chrono::Utc::now().to_rfc3339()
            }
        }
    });
    fs::write(&cache_file, serde_json::to_string(&missing_cache).unwrap()).unwrap();
    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&cache_file).unwrap().permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&cache_file, perms).unwrap();
    }

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(val["results"][0]["cached"], false);
    assert_eq!(val["results"][0]["source"], "probe");
    assert_eq!(fs::read_to_string(&counter_file).unwrap(), "1");
}

#[test]
fn test_quota_cache_write_failure_surfaced_as_error() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    // Create cache_path as an unreadable/unwritable directory to force cache write failure
    let cache_path = profiles_temp.path().join("quota-cache.json");
    fs::create_dir(&cache_path).unwrap();

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    // Cache write/security failure must surface status error and exit 1
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(val["schema_version"], 1);
    assert_eq!(val["results"][0]["status"], "error");
    assert_eq!(
        val["results"][0]["detail"].as_str().unwrap(),
        "Quota cache operation failed."
    );
}

// -------------------------------------------------------------
// 6. Keychain Safety & Exact Restoration
// -------------------------------------------------------------

#[test]
fn test_quota_exact_keychain_restoration() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
# Check that active token is the profile token during probe
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    // Initial token was "initial-system-token"
    assert_eq!(
        fs::read_to_string(&mock_keychain).unwrap(),
        "initial-system-token"
    );

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));

    // After quota probe completes, Keychain token must be EXACTLY restored!
    assert_eq!(
        fs::read_to_string(&mock_keychain).unwrap(),
        "initial-system-token",
        "Keychain token must be restored to prior state!"
    );
}

#[test]
fn test_quota_all_restoration_failure_stops_immediately() {
    use commands::quota::{execute_quota_with_backend, MockProbeRunner};
    use keychain::KeychainBackend;

    // A stateful backend that fails restoration write
    #[derive(Clone)]
    struct FailingRestoreBackend {
        token: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        write_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl KeychainBackend for FailingRestoreBackend {
        fn read_token(&self) -> Result<Option<String>, String> {
            Ok(self.token.lock().unwrap().clone())
        }
        fn write_token(&self, token: &str) -> Result<(), String> {
            let count = self
                .write_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                // First write is activation -> succeed
                *self.token.lock().unwrap() = Some(token.to_string());
                Ok(())
            } else {
                // Subsequent write is restoration -> FAIL!
                Err("Simulated Keychain restoration error".to_string())
            }
        }
        fn delete_token(&self) -> Result<(), String> {
            Err("Simulated delete failure".to_string())
        }
    }

    let profiles_temp = tempdir().unwrap();
    setup_test_profile(profiles_temp.path(), "p1", "p1@example.com");
    setup_test_profile(profiles_temp.path(), "p2", "p2@example.com");
    setup_test_profile(profiles_temp.path(), "p3", "p3@example.com");

    let _lock = lock_test_env();
    let _env_guard = EnvVarGuard::set("AGYP_PROFILES_DIR", profiles_temp.path());

    let backend = FailingRestoreBackend {
        token: std::sync::Arc::new(std::sync::Mutex::new(Some("orig".to_string()))),
        write_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    let probe_runner = MockProbeRunner::new(Ok(agy::CapturedOutput {
        status: None,
        stdout: b"PONG\n".to_vec(),
        stderr: Vec::new(),
        timed_out: false,
    }));

    let (res, json_doc) = execute_quota_with_backend(
        None, // --all
        true,
        Some("30s"),
        true, // no-cache
        backend,
        probe_runner,
    );

    // Restoration failure must fail with exit code 1
    assert_eq!(res, Err(1));
    assert_eq!(
        json_doc.results.len(),
        1,
        "Must stop immediately on restoration failure! p2 and p3 must not run."
    );
    assert_eq!(json_doc.results[0]["profile"], "p1");
    assert_eq!(json_doc.results[0]["status"], "error");
    assert!(json_doc.results[0]["detail"]
        .as_str()
        .unwrap()
        .contains("Keychain restoration failed"));
}

// -------------------------------------------------------------
// 7. Exit Code Contract
// -------------------------------------------------------------

#[test]
fn test_quota_exit_code_contract() {
    use commands::quota::{calculate_quota_exit_code, QuotaStatus};

    // 1. All available -> 0
    assert_eq!(calculate_quota_exit_code(&[QuotaStatus::Available]), 0);
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::Available, QuotaStatus::Available]),
        0
    );

    // 2. Any quota_exhausted and no errors -> 2
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::Available, QuotaStatus::QuotaExhausted]),
        2
    );

    // 3. Any unauthenticated and no errors -> 2
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::Available, QuotaStatus::Unauthenticated]),
        2
    );
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::QuotaExhausted, QuotaStatus::Unauthenticated]),
        2
    );

    // 4. Any error/unknown/unsupported -> 1 takes precedence!
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::Available, QuotaStatus::Error]),
        1
    );
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::QuotaExhausted, QuotaStatus::Error]),
        1
    );
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::Available, QuotaStatus::Unknown]),
        1
    );
    assert_eq!(
        calculate_quota_exit_code(&[QuotaStatus::Available, QuotaStatus::Unsupported]),
        1
    );
}

// -------------------------------------------------------------
// 8. Security: No Skip Permissions, No Env Backdoors
// -------------------------------------------------------------

#[test]
fn test_quota_no_skip_permissions() {
    let quota_src = include_str!("../src/commands/quota.rs");
    assert!(
        !quota_src.contains("--dangerously-skip-permissions"),
        "quota probe must never use --dangerously-skip-permissions!"
    );
}

#[test]
fn test_no_production_test_env_hooks_in_quota() {
    let quota_src = include_str!("../src/commands/quota.rs");
    assert!(
        !quota_src.contains("AGYP_TEST_"),
        "No test backdoor environment variables allowed in src/commands/quota.rs"
    );
}

// -------------------------------------------------------------
// 9. Deadline, Emergency Restore Budget, and CLI Precedence
// -------------------------------------------------------------

#[test]
fn test_quota_no_further_probes_after_total_deadline() {
    use commands::quota::execute_quota_with_backend;
    use keychain::MockKeychainBackend;

    let profiles_temp = tempdir().unwrap();
    setup_test_profile(profiles_temp.path(), "p1", "p1@example.com");
    setup_test_profile(profiles_temp.path(), "p2", "p2@example.com");

    let _lock = lock_test_env();
    let _env_guard = EnvVarGuard::set("AGYP_PROFILES_DIR", profiles_temp.path());

    let backend = MockKeychainBackend::new(Some("orig_token".to_string()));

    #[derive(Clone)]
    struct DelayProbeRunner {
        count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl commands::quota::QuotaProbeRunner for DelayProbeRunner {
        fn run_probe(
            &self,
            _profile_dir: &Path,
            _timeout: Option<std::time::Duration>,
        ) -> Result<agy::CapturedOutput, String> {
            self.count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(80));
            Ok(agy::CapturedOutput {
                status: None,
                stdout: b"PONG\n".to_vec(),
                stderr: Vec::new(),
                timed_out: false,
            })
        }
    }

    let runner = DelayProbeRunner {
        count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };

    // Overall deadline is 40ms; p1 takes 80ms so deadline expires. p2 must NOT be probed!
    let (res, json_doc) =
        execute_quota_with_backend(None, true, Some("40ms"), true, backend, runner.clone());

    assert_eq!(res, Err(1));
    assert_eq!(
        runner.count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "Probe for p2 must never execute after total deadline!"
    );
    assert_eq!(
        json_doc.results.len(),
        1,
        "Results must only contain the profile that reached deadline"
    );
    assert_eq!(json_doc.results[0]["status"], "error");
}

#[test]
fn test_cli_quota_unauthenticated_profile() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();

    let profile_dir = profiles_temp.path().join("profiles").join("unauth_profile");
    fs::create_dir_all(&profile_dir).unwrap();
    let config_file = profiles_temp.path().join("profiles.json");
    let mut reg = ProfileRegistry::load_or_default(&config_file).unwrap();
    reg.add_profile(ProfileInfo {
        name: "unauth_profile".to_string(),
        email: "unauth@example.com".to_string(),
        dir: profile_dir,
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.save(&config_file).unwrap();

    let out = Command::new(bin)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "unauth_profile", "--json"])
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(2),
        "Unauthenticated profile must exit 2"
    );
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(val["results"][0]["status"], "unauthenticated");
    assert_eq!(val["results"][0]["source"], "preflight");
    assert_eq!(val["results"][0]["profile"], "unauth_profile");
    assert_eq!(val["results"][0]["email"], "unauth@example.com");
}

#[test]
fn test_cli_quota_exit_precedence_mixed() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();

    setup_test_profile(profiles_temp.path(), "a_avail", "avail@example.com");
    let unauth_dir = profiles_temp.path().join("profiles").join("z_unauth");
    fs::create_dir_all(&unauth_dir).unwrap();
    let config_file = profiles_temp.path().join("profiles.json");
    let mut reg = ProfileRegistry::load_or_default(&config_file).unwrap();
    reg.add_profile(ProfileInfo {
        name: "z_unauth".to_string(),
        email: "unauth@example.com".to_string(),
        dir: unauth_dir,
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.save(&config_file).unwrap();

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let mock_keychain = profiles_temp.path().join("mock_system_keychain.json");
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    // Available + Unauthenticated -> exit 2!
    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--all", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    let results = val["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["status"], "available");
    assert_eq!(results[0]["source"], "probe");
    assert_eq!(results[1]["status"], "unauthenticated");
    assert_eq!(results[1]["source"], "preflight");
    assert_eq!(val["source"], "mixed");
}

#[test]
fn test_quota_json_exact_fields_and_types() {
    let _lock = lock_test_env();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "PONG"
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let path_env = format!("{}:{}", bin_dir.display(), orig_path);

    let out = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args(["quota", "--profile", "dev", "--json"])
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0));
    assert!(
        out.stderr.is_empty(),
        "stderr must be empty in clean JSON run"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout).unwrap();

    // Top-level types
    assert_eq!(val["schema_version"].as_u64().unwrap(), 1);
    assert!(chrono::DateTime::parse_from_rfc3339(val["checked_at"].as_str().unwrap()).is_ok());
    assert_eq!(val["source"].as_str().unwrap(), "probe");

    let results = val["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    let item = &results[0];
    assert_eq!(item["profile"].as_str().unwrap(), "dev");
    assert_eq!(item["email"].as_str().unwrap(), "dev@example.com");
    assert_eq!(item["status"].as_str().unwrap(), "available");
    assert!(chrono::DateTime::parse_from_rfc3339(item["observed_at"].as_str().unwrap()).is_ok());
    assert!(!item["cached"].as_bool().unwrap());
    assert_eq!(item["source"].as_str().unwrap(), "probe");
    assert_eq!(
        item["detail"].as_str().unwrap(),
        "Probe completed successfully"
    );
    assert_eq!(item["agent_exit_code"].as_i64().unwrap(), 0);
}
