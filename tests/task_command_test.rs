use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;

#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
mod commands;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/keychain.rs"]
mod keychain;
#[path = "../src/security.rs"]
mod security;

use agy::run_command_captured_interruptibly_with_timeout;
use commands::task::{
    classify_task_outcome, parse_duration, read_prompt_file_safely, read_prompt_from_opened_file,
    resolve_task_prompt_with_timeout, validate_linked_worktree, ResolvePromptError,
    TaskFailureKind, MAX_PROMPT_FILE_SIZE,
};
use config::{ProfileInfo, ProfileRegistry};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

fn create_test_git_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempdir().unwrap();
    let main_repo = temp.path().join("main_repo");
    let worktree = temp.path().join("linked_worktree");
    fs::create_dir_all(&main_repo).unwrap();

    let run_git = |args: &[&str], dir: &Path| {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git command failed: {:?}, stderr: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run_git(&["init"], &main_repo);
    run_git(&["config", "user.email", "test@example.com"], &main_repo);
    run_git(&["config", "user.name", "Test User"], &main_repo);
    fs::write(main_repo.join("README.md"), "hello").unwrap();
    run_git(&["add", "README.md"], &main_repo);
    run_git(&["commit", "-m", "init"], &main_repo);

    run_git(
        &[
            "worktree",
            "add",
            "-b",
            "test-branch",
            worktree.to_str().unwrap(),
        ],
        &main_repo,
    );

    (temp, main_repo, worktree)
}

fn create_mock_agy_script(dir: &Path, script_content: &str) -> PathBuf {
    let script_path = dir.join("mock_agy.sh");
    let mut file = fs::File::create(&script_path).unwrap();
    file.write_all(script_content.as_bytes()).unwrap();
    let mut perms = file.metadata().unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&script_path, perms).unwrap();
    script_path
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

#[test]
fn test_parse_duration() {
    assert_eq!(parse_duration("10").unwrap(), Duration::from_secs(10));
    assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
    assert_eq!(parse_duration("20m").unwrap(), Duration::from_secs(1200));
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3600));
    assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));

    assert!(parse_duration("0").is_err());
    assert!(parse_duration("0s").is_err());
    assert!(parse_duration("-5s").is_err());
    assert!(parse_duration("abc").is_err());
    assert!(parse_duration("").is_err());
}

#[test]
fn test_classify_task_outcome() {
    // 1. Timeout
    assert_eq!(
        classify_task_outcome(true, None, "", ""),
        Err(TaskFailureKind::Timeout)
    );

    // 2. Quota / 429 in stderr
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "some output",
            "Error: 429 RESOURCE_EXHAUSTED"
        ),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "rate limit exceeded (429)"),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "prose", "Your quota reached limit"),
        Err(TaskFailureKind::QuotaExhausted)
    );

    // 3. Permission denied in stderr
    assert_eq!(
        classify_task_outcome(false, Some(0), "some output", "Permission denied by user"),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "tool call rejected: permission_denied"),
        Err(TaskFailureKind::PermissionDenied)
    );

    // 4. Exit 0 but empty stdout
    assert_eq!(
        classify_task_outcome(false, Some(0), "", ""),
        Err(TaskFailureKind::EmptyOutput)
    );
    assert_eq!(
        classify_task_outcome(false, Some(0), "   \n\t  ", ""),
        Err(TaskFailureKind::EmptyOutput)
    );

    // 5. Exit != 0 with no special markers -> AgentFailed
    assert_eq!(
        classify_task_outcome(false, Some(1), "some failure", "internal err"),
        Err(TaskFailureKind::AgentFailed)
    );

    // 6. Success
    assert_eq!(
        classify_task_outcome(false, Some(0), "successfully generated code", ""),
        Ok(())
    );
}

#[test]
fn test_validate_linked_worktree() {
    let (_temp, main_repo, worktree) = create_test_git_worktree();

    // Valid linked worktree
    let canonical = validate_linked_worktree(&worktree).unwrap();
    assert_eq!(canonical, worktree.canonicalize().unwrap());

    // Main repo checkout must be rejected
    let err_main = validate_linked_worktree(&main_repo).unwrap_err();
    assert!(err_main.contains("not a linked Git worktree") || err_main.contains("main repository"));

    // Subdirectory inside linked worktree must be rejected
    let sub_dir = worktree.join("some_subdir");
    fs::create_dir_all(&sub_dir).unwrap();
    let err_sub = validate_linked_worktree(&sub_dir).unwrap_err();
    assert!(
        err_sub.to_lowercase().contains("not a git worktree")
            || err_sub.contains("exact Git toplevel")
    );

    // Non-existent directory
    let non_existent = worktree.join("does_not_exist");
    assert!(validate_linked_worktree(&non_existent).is_err());

    // Symlink to worktree must be rejected
    let symlink_path = _temp.path().join("worktree_symlink");
    std::os::unix::fs::symlink(&worktree, &symlink_path).unwrap();
    let err_symlink = validate_linked_worktree(&symlink_path).unwrap_err();
    assert!(err_symlink.contains("symlink"));
}

#[test]
fn test_validate_linked_worktree_rejects_forged_gitdir() {
    let (_temp, main_repo, _worktree) = create_test_git_worktree();
    let forged_dir = _temp.path().join("forged_worktree");
    fs::create_dir_all(&forged_dir).unwrap();
    // Forge .git file pointing directly to main repo's .git
    let dot_git = forged_dir.join(".git");
    fs::write(
        &dot_git,
        format!("gitdir: {}\n", main_repo.join(".git").display()),
    )
    .unwrap();

    let result = validate_linked_worktree(&forged_dir);
    assert!(
        result.is_err(),
        "Forged worktree pointing to main repo's .git must be rejected!"
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("registered linked worktree")
            || err.contains("not a linked Git worktree")
            || err.contains("forged")
            || err.contains("main repository"),
        "Unexpected error message: {err}"
    );
}

#[test]
fn test_classify_task_outcome_no_false_positives() {
    // Prose containing "429" on exit 0 must succeed
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "fixed HTTP 429 handling in network layer",
            ""
        ),
        Ok(())
    );
    // Prose containing "permission denied" on exit 0 must succeed
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "prior permission denied error resolved successfully",
            ""
        ),
        Ok(())
    );
    // Even stderr prose about tests/docs must not trigger an agent failure.
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "implemented error handling",
            "Added handling for RESOURCE_EXHAUSTED and error: 429 responses"
        ),
        Ok(())
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "implemented permission handling",
            "Test error: permission denied responses are now covered"
        ),
        Ok(())
    );
}

#[test]
fn test_descendant_held_pipe_hang_terminates_on_deadline() {
    let temp = tempdir().unwrap();
    let script = r#"#!/bin/sh
printf 'parent stdout before exit\n'
(sleep 30; echo "descendant finished") &
exit 0
"#;
    let script_path = create_mock_agy_script(temp.path(), script);
    let mut cmd = Command::new(&script_path);
    let start = std::time::Instant::now();
    let output = run_command_captured_interruptibly_with_timeout(
        &mut cmd,
        Some(Duration::from_millis(1500)),
    )
    .unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "Descendant pipe hang did not respect deadline! Took {:?}",
        elapsed
    );
    assert!(output.timed_out, "Should be marked timed_out");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("parent stdout before exit"),
        "Partial stdout should be preserved"
    );
}

#[test]
fn test_pipe_buffer_concurrent_capture() {
    // Generate > 128KB stdout and > 128KB stderr concurrently to verify no OS pipe deadlock
    let script = r#"#!/bin/sh
python3 -c '
import sys
chunk = "A" * 8192
for _ in range(16):
    sys.stdout.write(chunk)
    sys.stderr.write(chunk)
sys.stdout.flush()
sys.stderr.flush()
'
"#;
    let temp = tempdir().unwrap();
    let script_path = create_mock_agy_script(temp.path(), script);

    let mut cmd = Command::new(&script_path);
    let output =
        run_command_captured_interruptibly_with_timeout(&mut cmd, Some(Duration::from_secs(10)))
            .unwrap();

    assert!(!output.timed_out);
    assert_eq!(output.status.and_then(|s| s.code()), Some(0));
    assert_eq!(output.stdout.len(), 16 * 8192);
    assert_eq!(output.stderr.len(), 16 * 8192);
}

#[test]
fn test_cmd_task_cli_prompt_file_and_stdin() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "task executed successfully with args: $@"
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("task_prompt.txt");
    fs::write(&prompt_file, "Fix bug in module X").unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");

    // 1. Invocation with --prompt-file
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    let stdout_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(stdout_json["status"], "success");
    assert!(stdout_json["failure_kind"].is_null());
    assert_eq!(stdout_json["exit_code"], 0);
    assert_eq!(stdout_json["agent_exit_code"], 0);
    assert_eq!(stdout_json["cli_exit_code"], 0);
    assert_eq!(stdout_json["timed_out"], false);
    assert!(stdout_json["stdout"]
        .as_str()
        .unwrap()
        .contains("task executed successfully"));

    // 2. Invocation via stdin
    let mut child = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--json",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"Fix bug via stdin").unwrap();
    }
    let output2 = child.wait_with_output().unwrap();
    assert_eq!(output2.status.code(), Some(0));
    let stdout_json2: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output2.stdout)).unwrap();
    assert_eq!(stdout_json2["status"], "success");

    // 3. Empty prompt rejection
    let output_empty = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--json",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();

    assert_ne!(output_empty.status.code(), Some(0));
    let empty_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output_empty.stdout)).unwrap();
    assert_eq!(empty_json["status"], "error");
    assert_eq!(empty_json["failure_kind"], "invalid_prompt");
    assert_eq!(empty_json["cli_exit_code"], 1);
}

#[test]
fn test_cmd_task_exit_0_empty_stdout() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
exit 0
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("task_prompt.txt");
    fs::write(&prompt_file, "Do something").unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let stdout_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(stdout_json["status"], "error");
    assert_eq!(stdout_json["failure_kind"], "empty_output");
    assert_eq!(stdout_json["agent_exit_code"], 0);
    assert_eq!(stdout_json["cli_exit_code"], 1);
}

#[test]
fn test_cmd_task_quota_429() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "Error: 429 RESOURCE_EXHAUSTED" >&2
exit 1
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("task_prompt.txt");
    fs::write(&prompt_file, "Do something").unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let stdout_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(stdout_json["status"], "error");
    assert_eq!(stdout_json["failure_kind"], "quota_exhausted");
    assert_eq!(stdout_json["cli_exit_code"], 1);
}

#[test]
fn test_cmd_task_permission_denied() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "tool call rejected: permission_denied" >&2
exit 1
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("task_prompt.txt");
    fs::write(&prompt_file, "Do something").unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let stdout_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(stdout_json["status"], "error");
    assert_eq!(stdout_json["failure_kind"], "permission_denied");
    assert_eq!(stdout_json["cli_exit_code"], 1);
}

#[test]
fn test_cmd_task_timeout() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
printf 'starting long task\n'
sleep 10
printf 'finished long task\n'
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("task_prompt.txt");
    fs::write(&prompt_file, "Do something").unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--timeout",
            "1500ms",
            "--json",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let stdout_json: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(stdout_json["status"], "error");
    assert_eq!(stdout_json["failure_kind"], "timeout");
    assert_eq!(stdout_json["timed_out"], true);
    assert!(stdout_json["agent_exit_code"].is_null());
    assert_eq!(stdout_json["cli_exit_code"], 1);
}

#[test]
fn test_cmd_task_rejects_positional_prompt() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = Command::new(bin)
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "unexpected positional prompt",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unexpected argument") || stderr.contains("Usage:"));
}

#[test]
fn test_cmd_task_human_mode_success_and_failure() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script_success = r#"#!/bin/sh
echo "All tests pass successfully!"
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script_success);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("task_prompt.txt");
    fs::write(&prompt_file, "Refactor code").unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");

    // Human mode success
    let output_success = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output_success.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output_success.stdout);
    assert!(stdout.contains("All tests pass successfully!"));

    // Human mode failure (rate limit 429)
    let script_fail = r#"#!/bin/sh
echo "RESOURCE_EXHAUSTED: rate limit" >&2
exit 1
"#;
    let temp_bin2 = tempdir().unwrap();
    let bin_dir2 = setup_test_bin_dir(temp_bin2.path(), &mock_keychain, script_fail);
    let path_env2 = format!(
        "{}:{}",
        bin_dir2.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output_fail = Command::new(bin)
        .env("PATH", &path_env2)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_ne!(output_fail.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output_fail.stderr);
    assert!(stderr.contains("RESOURCE_EXHAUSTED"));
    assert!(stderr.contains("Error [quota_exhausted]"));
}

// -------------------------------------------------------------
// Regression Tests for Review Findings 2, 4, 5, 6, 7, 8, 10, 11, 12, 13
// -------------------------------------------------------------

#[test]
fn test_parse_security_find_output_tri_state() {
    use keychain::parse_security_find_output;

    // 1. Success exit 0 with token
    let res = parse_security_find_output(Some(0), b"my_token\n", b"");
    assert_eq!(res, Ok(Some("my_token".to_string())));

    // 2. Success exit 0 but empty stdout -> Absent
    let res_empty = parse_security_find_output(Some(0), b"  \n", b"");
    assert_eq!(res_empty, Ok(None));

    // 3. Exit 44 -> Absent (item not found)
    let res_44 = parse_security_find_output(
        Some(44),
        b"",
        b"security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain.\n",
    );
    assert_eq!(res_44, Ok(None));

    // 4. Stderr contains item not found -> Absent
    let res_not_found = parse_security_find_output(
        Some(1),
        b"",
        b"The specified item could not be found in the keychain",
    );
    assert_eq!(res_not_found, Ok(None));

    // 5. Exit 36 (locked / user interaction not allowed) -> Err
    let res_locked = parse_security_find_output(
        Some(36),
        b"",
        b"security: SecKeychainSearchCopyNext: User interaction is not allowed.\n",
    );
    assert!(res_locked.is_err());
    assert!(res_locked.unwrap_err().contains("code 36"));

    // 6. Permission denied / other error -> Err
    let res_perm = parse_security_find_output(Some(1), b"", b"security: Permission denied\n");
    assert!(res_perm.is_err());

    // 7. Killed by signal -> Err
    let res_sig = parse_security_find_output(None, b"", b"terminated");
    assert!(res_sig.is_err());
}

#[test]
fn test_keychain_snapshot_tri_state_aborts_before_activation() {
    use keychain::{KeychainGuard, MockKeychainBackend};

    let temp = tempdir().unwrap();
    let profile_dir = temp.path().join("p1");
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(
        profile_dir.join("keychain_token.json"),
        r#"{"token":{"id_token":"dummy"},"email":"p1@example.com"}"#,
    )
    .unwrap();

    let mock_backend = MockKeychainBackend::new(Some("system_initial".to_string()));
    // Set fail_read to simulate locked keychain / permission error
    mock_backend
        .fail_read
        .store(true, std::sync::atomic::Ordering::SeqCst);

    let guard_res = KeychainGuard::new_with_backend(&profile_dir, mock_backend.clone());
    assert!(
        guard_res.is_err(),
        "Must abort before activation on read error!"
    );

    // Verify no write or delete call was made!
    let calls = mock_backend.calls.lock().unwrap().clone();
    assert_eq!(calls, vec!["read".to_string()]);
    // System token must remain completely untouched!
    assert_eq!(
        *mock_backend.token.lock().unwrap(),
        Some("system_initial".to_string())
    );
}

#[test]
fn test_lock_setup_overall_timeout() {
    use commands::task::cmd_task_with_backend;
    use keychain::MockKeychainBackend;

    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, _mock_kc) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let prompt_file = profiles_temp.path().join("prompt.txt");
    fs::write(&prompt_file, "Do task").unwrap();

    // Acquire lock in this thread and keep it held
    let lock_path = profiles_temp.path().join("agyp.lock");
    fs::create_dir_all(profiles_temp.path()).unwrap();
    let _held_lock = security::ProcessLock::acquire(&lock_path).unwrap();

    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvVarGuard::set("AGYP_PROFILES_DIR", profiles_temp.path());
    let mock_backend = MockKeychainBackend::new(Some("system_token".to_string()));
    let start = std::time::Instant::now();

    // Task with 300ms timeout while lock is held
    let res = cmd_task_with_backend(
        "dev",
        &worktree,
        Some(&prompt_file),
        Some("300ms"),
        None,
        false,
        false,
        mock_backend,
    );
    let elapsed = start.elapsed();

    assert!(res.is_err());
    assert!(
        elapsed < Duration::from_secs(3),
        "Lock wait exceeded deadline! Took {:?}",
        elapsed
    );
}

#[derive(Clone, Default)]
struct StatefulRestoreFailBackend {
    token: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    write_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl StatefulRestoreFailBackend {
    fn new(initial_token: Option<String>) -> Self {
        Self {
            token: std::sync::Arc::new(std::sync::Mutex::new(initial_token)),
            write_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }
}

impl keychain::KeychainBackend for StatefulRestoreFailBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        Ok(self.token.lock().unwrap().clone())
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        let count = self
            .write_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if count >= 2 {
            // Fails on restore! Note that the error message contains "write rejected"
            // which also tests Finding 4 (ensuring restore text does not populate agent_failure_kind).
            return Err("Simulated restore write failure (keychain write rejected)".to_string());
        }
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn delete_token(&self) -> Result<(), String> {
        let count = self
            .write_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if count >= 2 {
            return Err("Simulated restore delete failure".to_string());
        }
        *self.token.lock().unwrap() = None;
        Ok(())
    }
}

#[test]
fn test_restore_failure_precedence_over_agent_failure() {
    use commands::task::execute_task_with_backend;

    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, _mock_kc) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let marker_file = worktree.join("child_ran.txt");
    let script = format!(
        r#"#!/bin/sh
printf "executed" > "{}"
echo "Agent execution failed unexpectedly" >&2
exit 1
"#,
        marker_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(
        temp_bin.path(),
        &profiles_temp.path().join("kc.json"),
        &script,
    );
    let prompt_file = profiles_temp.path().join("prompt.txt");
    fs::write(&prompt_file, "Fix bug").unwrap();

    let stateful_backend = StatefulRestoreFailBackend::new(Some("system_orig".to_string()));

    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvVarGuard::set("AGYP_PROFILES_DIR", profiles_temp.path());
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let _path_guard = EnvVarGuard::set("PATH", format!("{}:{}", bin_dir.display(), orig_path));

    let (res, json_output) = execute_task_with_backend(
        "dev",
        &worktree,
        Some(&prompt_file),
        Some("10s"),
        None,
        false,
        stateful_backend,
    );

    // 1. Child really ran (marker file was written)
    assert!(
        marker_file.exists(),
        "Child process was never executed! Activation must succeed so child runs before restore fails."
    );

    // 2. CLI exit code is failure
    assert!(res.is_err(), "Must be CLI error when restore fails");
    assert_eq!(res.unwrap_err(), 1);

    // 3. Exact JSON assertions:
    // failure_kind is setup_error (precedence!)
    // agent_failure_kind is agent_failed (from original child stderr/exit status)
    // restore_error is non-null and retains details
    // exit_code and agent_exit_code preserve child's exit code 1
    assert_eq!(json_output.status, "error");
    assert_eq!(json_output.failure_kind, Some("setup_error".to_string()));
    assert_eq!(
        json_output.agent_failure_kind,
        Some("agent_failed".to_string())
    );
    assert!(json_output.restore_error.is_some());
    assert!(json_output
        .restore_error
        .as_ref()
        .unwrap()
        .contains("Simulated restore write failure"));
    assert_eq!(json_output.exit_code, Some(1));
    assert_eq!(json_output.agent_exit_code, Some(1));
    assert_eq!(json_output.cli_exit_code, 1);
}

#[test]
fn test_restore_failure_precedence_when_child_succeeds() {
    use commands::task::execute_task_with_backend;

    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, _mock_kc) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let marker_file = worktree.join("child_succeeded.txt");
    let script = format!(
        r#"#!/bin/sh
printf "executed" > "{}"
echo "Child executed successfully with valid prose"
exit 0
"#,
        marker_file.display()
    );

    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(
        temp_bin.path(),
        &profiles_temp.path().join("kc.json"),
        &script,
    );
    let prompt_file = profiles_temp.path().join("prompt.txt");
    fs::write(&prompt_file, "Fix bug").unwrap();

    let stateful_backend = StatefulRestoreFailBackend::new(Some("system_orig".to_string()));

    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvVarGuard::set("AGYP_PROFILES_DIR", profiles_temp.path());
    let orig_path = std::env::var("PATH").unwrap_or_default();
    let _path_guard = EnvVarGuard::set("PATH", format!("{}:{}", bin_dir.display(), orig_path));

    let (res, json_output) = execute_task_with_backend(
        "dev",
        &worktree,
        Some(&prompt_file),
        Some("10s"),
        None,
        false,
        stateful_backend,
    );

    assert!(marker_file.exists());
    assert!(res.is_err());
    assert_eq!(res.unwrap_err(), 1);

    // Finding 4: When child succeeded, agent_failure_kind must be None!
    // Never let restore error text (which may contain "rejected" or "denied") populate agent_failure_kind!
    assert_eq!(json_output.status, "error");
    assert_eq!(json_output.failure_kind, Some("setup_error".to_string()));
    assert_eq!(json_output.agent_failure_kind, None);
    assert!(json_output.restore_error.is_some());
    assert_eq!(json_output.exit_code, Some(0));
    assert_eq!(json_output.agent_exit_code, Some(0));
    assert_eq!(json_output.cli_exit_code, 1);
}

#[test]
fn test_cli_json_for_missing_required_arguments_and_invalid_timeout() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let bin = env!("CARGO_BIN_EXE_agyp");

    // 1. Missing --profile with --json
    let out1 = Command::new(bin)
        .args(["task", "--worktree", worktree.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_ne!(out1.status.code(), Some(0));
    let j1: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out1.stdout)).unwrap();
    assert_eq!(j1["status"], "error");
    assert_eq!(j1["failure_kind"], "invalid_profile");
    assert_eq!(j1["cli_exit_code"], 1);

    // 2. Missing --worktree with --json
    let out2 = Command::new(bin)
        .args(["task", "--profile", "dev", "--json"])
        .output()
        .unwrap();
    assert_ne!(out2.status.code(), Some(0));
    let j2: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out2.stdout)).unwrap();
    assert_eq!(j2["status"], "error");
    assert_eq!(j2["failure_kind"], "invalid_worktree");
    assert_eq!(j2["cli_exit_code"], 1);

    // 3. Invalid timeout with --json
    let out3 = Command::new(bin)
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--timeout",
            "invalid_time",
            "--json",
        ])
        .output()
        .unwrap();
    assert_ne!(out3.status.code(), Some(0));
    let j3: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out3.stdout)).unwrap();
    assert_eq!(j3["status"], "error");
    assert_eq!(j3["failure_kind"], "invalid_duration");
    assert_eq!(j3["cli_exit_code"], 1);
}

#[test]
fn test_task_rejects_oauth_only_and_jetski_only_profiles() {
    use commands::task::cmd_task_with_backend;
    use keychain::MockKeychainBackend;

    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();

    // 1. Profile with ONLY oauth_creds.json (no keychain_token.json)
    let p_oauth_dir = profiles_temp.path().join("profiles").join("oauth_only");
    fs::create_dir_all(&p_oauth_dir).unwrap();
    fs::write(
        p_oauth_dir.join("oauth_creds.json"),
        r#"{"access_token":"ya29.fake"}"#,
    )
    .unwrap();

    // 2. Profile with ONLY jetski-standalone-oauth-token (no keychain_token.json)
    let p_jetski_dir = profiles_temp.path().join("profiles").join("jetski_only");
    fs::create_dir_all(&p_jetski_dir).unwrap();
    fs::write(
        p_jetski_dir.join("jetski-standalone-oauth-token"),
        "token-string",
    )
    .unwrap();

    let config_file = profiles_temp.path().join("profiles.json");
    let mut reg = ProfileRegistry::load_or_default(&config_file).unwrap();
    reg.add_profile(ProfileInfo {
        name: "oauth_only".to_string(),
        email: "oauth@example.com".to_string(),
        dir: p_oauth_dir,
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.add_profile(ProfileInfo {
        name: "jetski_only".to_string(),
        email: "jetski@example.com".to_string(),
        dir: p_jetski_dir,
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.save(&config_file).unwrap();

    let _lock = ENV_LOCK.lock().unwrap();
    let _env_guard = EnvVarGuard::set("AGYP_PROFILES_DIR", profiles_temp.path());
    let prompt_file = profiles_temp.path().join("prompt.txt");
    fs::write(&prompt_file, "Do task").unwrap();

    let mock_backend = MockKeychainBackend::new(Some("sys".to_string()));

    // Test oauth_only
    let res1 = cmd_task_with_backend(
        "oauth_only",
        &worktree,
        Some(&prompt_file),
        None,
        None,
        false,
        false,
        mock_backend.clone(),
    );
    assert_eq!(res1, Err(2), "Unauthenticated must return exit code 2");

    // Test jetski_only
    let res2 = cmd_task_with_backend(
        "jetski_only",
        &worktree,
        Some(&prompt_file),
        None,
        None,
        false,
        false,
        mock_backend,
    );
    assert_eq!(res2, Err(2), "Unauthenticated must return exit code 2");
}

#[test]
fn test_task_json_exit_codes_consistency() {
    use commands::task::TaskJsonOutput;
    let out = TaskJsonOutput {
        profile: "dev".to_string(),
        worktree: "./wt".to_string(),
        canonical_worktree: Some("/abs/wt".to_string()),
        status: "success".to_string(),
        failure_kind: None,
        agent_failure_kind: None,
        restore_error: None,
        exit_code: Some(0),
        agent_exit_code: Some(0),
        cli_exit_code: 0,
        timed_out: false,
        duration_ms: 100,
        stdout: "ok".to_string(),
        stderr: "".to_string(),
    };
    let val: serde_json::Value = serde_json::to_value(&out).unwrap();
    assert_eq!(val["agent_exit_code"], 0);
    assert_eq!(val["cli_exit_code"], 0);
    assert_eq!(val["canonical_worktree"], "/abs/wt");
    assert_eq!(val["worktree"], "./wt");
}

#[test]
fn test_prompt_forwarding_multiline_and_option_like() {
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
python3 -c '
import sys
args = sys.argv
if "-p" in args:
    p_val = args[args.index("-p") + 1]
    with open("received_prompt.txt", "w") as f:
        f.write(p_val)
print("done")
' "$@"
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);

    let tricky_prompt = "--dangerously-skip-permissions\n--flag=value\nLine 3 with 'single' and \"double\" quotes\n$VAR `backticks`";
    let prompt_file = profiles_temp.path().join("tricky_prompt.txt");
    fs::write(&prompt_file, tricky_prompt).unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // 1. Forwarding via --prompt-file
    let output1 = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .current_dir(&worktree)
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(output1.status.code(), Some(0));
    let received1 = fs::read_to_string(worktree.join("received_prompt.txt")).unwrap();
    assert_eq!(
        received1, tricky_prompt,
        "Multiline and option-like prompt must match exactly via prompt file!"
    );

    fs::remove_file(worktree.join("received_prompt.txt")).unwrap();

    // 2. Forwarding via stdin
    let mut child = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .current_dir(&worktree)
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--json",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(tricky_prompt.as_bytes()).unwrap();
    }
    let output2 = child.wait_with_output().unwrap();
    assert_eq!(output2.status.code(), Some(0));
    let received2 = fs::read_to_string(worktree.join("received_prompt.txt")).unwrap();
    assert_eq!(
        received2, tricky_prompt,
        "Multiline and option-like prompt must match exactly via stdin!"
    );
}

#[test]
fn test_worktree_schema_preserves_raw_input_and_null_canonical_on_error() {
    let bin = env!("CARGO_BIN_EXE_agyp");
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let script = r#"#!/bin/sh
echo "ok"
"#;
    let temp_bin = tempdir().unwrap();
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let prompt_file = profiles_temp.path().join("prompt.txt");
    fs::write(&prompt_file, "task").unwrap();

    // Pass non-existent path
    let raw_input = "./does/not/exist/worktree";
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            raw_input,
            "--prompt-file",
            prompt_file.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(val["worktree"], raw_input, "Must preserve raw user input");
    assert!(
        val["canonical_worktree"].is_null(),
        "canonical_worktree must be null on failure"
    );
}

#[test]
fn test_mock_keychain_safety_and_exact_restoration() {
    use keychain::{KeychainGuard, MockKeychainBackend};

    let temp = tempdir().unwrap();
    let profile_dir = temp.path().join("profile");
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(
        profile_dir.join("keychain_token.json"),
        r#"{"token":{"id_token":"new_tok"},"email":"user@example.com"}"#,
    )
    .unwrap();

    // Case A: Initial token present
    {
        let mock_backend = MockKeychainBackend::new(Some("initial_sys_token".to_string()));
        {
            let guard =
                KeychainGuard::new_with_backend(&profile_dir, mock_backend.clone()).unwrap();
            // During task, active token is the profile's token
            assert_eq!(
                *mock_backend.token.lock().unwrap(),
                Some(r#"{"token":{"id_token":"new_tok"},"email":"user@example.com"}"#.to_string())
            );
            guard.sync_and_restore().unwrap();
        }
        // After guard restore: exact initial token must be restored!
        assert_eq!(
            *mock_backend.token.lock().unwrap(),
            Some("initial_sys_token".to_string())
        );
        let calls = mock_backend.calls.lock().unwrap().clone();
        assert!(calls.contains(&"read".to_string()));
        assert!(calls.contains(&"write:initial_sys_token".to_string()));
    }

    // Case B: Initial token absent (None)
    {
        let mock_backend = MockKeychainBackend::new(None);
        {
            let guard =
                KeychainGuard::new_with_backend(&profile_dir, mock_backend.clone()).unwrap();
            guard.sync_and_restore().unwrap();
        }
        // After guard restore: deleted, back to None!
        assert_eq!(*mock_backend.token.lock().unwrap(), None);
        let calls = mock_backend.calls.lock().unwrap().clone();
        assert!(calls.contains(&"delete".to_string()));
    }
}

#[test]
fn test_no_production_test_env_hooks() {
    let keychain_src = include_str!("../src/keychain.rs");
    assert!(
        !keychain_src.contains("AGYP_TEST_KEYCHAIN_FILE"),
        "AGYP_TEST_KEYCHAIN_FILE must not exist in src/keychain.rs"
    );

    let agy_src = include_str!("../src/agy.rs");
    assert!(
        !agy_src.contains("AGYP_AGY_BIN"),
        "AGYP_AGY_BIN must not exist in src/agy.rs"
    );
}

// -------------------------------------------------------------
// Second-pass Regression Tests (Findings 1 - 8)
// -------------------------------------------------------------

#[test]
fn test_detached_descendant_setsid_bounded_return_and_partial_output() {
    // Finding 1: Descendant calls setsid() and retains stdout/stderr.
    // Reader threads must not hang indefinitely; return must be bounded and partial output preserved.
    let temp = tempdir().unwrap();
    let script = r#"#!/bin/sh
printf "partial stdout from setsid descendant\n"
printf "partial stderr from setsid descendant\n" >&2
/usr/bin/python3 -c '
import os, sys, time
pid = os.fork()
if pid == 0:
    os.setsid()
    time.sleep(15)
    sys.exit(0)
else:
    sys.exit(0)
'
"#;
    let script_path = create_mock_agy_script(temp.path(), script);
    let mut cmd = Command::new(&script_path);

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let res = run_command_captured_interruptibly_with_timeout(
            &mut cmd,
            Some(Duration::from_millis(1200)),
        );
        let elapsed = start.elapsed();
        let _ = tx.send((res, elapsed));
    });

    let (res, elapsed) = match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(val) => val,
        Err(_) => {
            panic!("Reader threads hung on setsid detached descendant! Bounded return violated.");
        }
    };

    let output = res.expect("Command execution should succeed");
    assert!(
        elapsed < Duration::from_secs(5),
        "Execution took too long: {:?}",
        elapsed
    );
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout_str.contains("partial stdout from setsid descendant"),
        "Partial stdout was lost! status: {:?}, timed_out: {}, elapsed: {:?}, stdout: {:?}, stderr: {:?}",
        output.status,
        output.timed_out,
        elapsed,
        stdout_str,
        stderr_str
    );
    assert!(
        stderr_str.contains("partial stderr from setsid descendant"),
        "Partial stderr was lost! status: {:?}, timed_out: {}, elapsed: {:?}, stdout: {:?}, stderr: {:?}",
        output.status,
        output.timed_out,
        elapsed,
        stdout_str,
        stderr_str
    );
}

#[test]
fn test_prompt_file_rejects_fifo_and_special_files() {
    // Finding 2: Require --prompt-file to be a regular file; reject FIFO.
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let fifo_path = profiles_temp.path().join("fifo_prompt");
    let status = Command::new("mkfifo").arg(&fifo_path).status().unwrap();
    assert!(status.success());

    let bin = env!("CARGO_BIN_EXE_agyp");
    let temp_bin = tempdir().unwrap();
    let script = r#"#!/bin/sh
echo "should not run"
"#;
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let (tx, rx) = std::sync::mpsc::channel();
    let fifo_clone = fifo_path.clone();
    let wt_clone = worktree.clone();
    let profiles_dir_clone = profiles_temp.path().to_path_buf();
    std::thread::spawn(move || {
        let out = Command::new(bin)
            .env("PATH", &path_env)
            .env("AGYP_PROFILES_DIR", &profiles_dir_clone)
            .args([
                "task",
                "--profile",
                "dev",
                "--worktree",
                wt_clone.to_str().unwrap(),
                "--prompt-file",
                fifo_clone.to_str().unwrap(),
                "--json",
            ])
            .output()
            .unwrap();
        let _ = tx.send(out);
    });

    let out = match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(o) => o,
        Err(_) => {
            panic!("Task hung on FIFO prompt file instead of rejecting it immediately!");
        }
    };

    assert_ne!(out.status.code(), Some(0));
    let json_val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(json_val["status"], "error");
    assert_eq!(json_val["failure_kind"], "invalid_prompt");
    assert!(
        json_val["stderr"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("regular file")
            || json_val["stderr"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("fifo"),
        "Error message should mention regular file/fifo: {}",
        json_val["stderr"]
    );
}

#[test]
fn test_prompt_stdin_non_eof_bounded_by_timeout() {
    // Finding 2: Stdin acquisition must be bounded by timeout when no EOF arrives.
    let (_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let bin = env!("CARGO_BIN_EXE_agyp");
    let temp_bin = tempdir().unwrap();
    let script = r#"#!/bin/sh
echo "should not run"
"#;
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let start = std::time::Instant::now();
    let mut child = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--timeout",
            "400ms",
            "--json",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    // Do NOT write to stdin and do NOT close stdin
    // Wait up to 3 seconds for the child to exit cleanly on its 400ms timeout
    let mut finished = false;
    for _ in 0..60 {
        if let Some(status) = child.try_wait().unwrap() {
            finished = true;
            assert_ne!(status.code(), Some(0));
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let elapsed = start.elapsed();
    if !finished {
        let _ = child.kill();
        let _ = child.wait();
        panic!("Stdin acquisition hung forever without EOF! Overall timeout was not enforced.");
    }

    assert!(
        elapsed < Duration::from_secs(3),
        "Stdin acquisition took too long: {:?}",
        elapsed
    );

    let output = child.wait_with_output().unwrap();
    let json_val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(json_val["status"], "error");
    assert_eq!(json_val["failure_kind"], "timeout");
    assert_eq!(json_val["timed_out"], true);
    assert_eq!(json_val["cli_exit_code"], 1);
}

#[test]
fn test_classifier_stdout_prose_containing_error_strings_remains_success() {
    // Finding 3: classify quota/permission ONLY from original child stderr.
    // Exit-0 stdout prose containing RESOURCE_EXHAUSTED, error: 429, error: permission denied must remain success!
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "I fixed the bug that produced RESOURCE_EXHAUSTED in the backend service.",
            ""
        ),
        Ok(())
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Patched retry handler for HTTP error: 429 responses.",
            ""
        ),
        Ok(())
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Resolved filesystem error: permission denied by granting correct access mode.",
            ""
        ),
        Ok(())
    );

    // Stderr anchored/documented signatures must be detected even on exit 0
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output before error",
            "429 RESOURCE_EXHAUSTED\n"
        ),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output before error",
            "Individual quota reached for project 12345\n"
        ),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output before error",
            "Error: permission_denied: user rejected tool execution\n"
        ),
        Err(TaskFailureKind::PermissionDenied)
    );
}

#[test]
fn test_cli_interception_subcommand_and_help_version() {
    // Finding 5: identify actual first subcommand only.
    // agyp task --help --json and --version normal Clap success semantics.
    let bin = env!("CARGO_BIN_EXE_agyp");

    // 1. agyp task --help --json must print help and exit 0
    let out_help = Command::new(bin)
        .args(["task", "--help", "--json"])
        .output()
        .unwrap();
    assert_eq!(
        out_help.status.code(),
        Some(0),
        "agyp task --help --json must exit 0! stderr: {}",
        String::from_utf8_lossy(&out_help.stderr)
    );
    let stdout_help = String::from_utf8_lossy(&out_help.stdout);
    assert!(
        stdout_help.contains("Usage:") || stdout_help.contains("task [OPTIONS]"),
        "Should display clap help: {stdout_help}"
    );

    // 2. agyp --version must print version and exit 0
    let out_ver = Command::new(bin).arg("--version").output().unwrap();
    assert_eq!(out_ver.status.code(), Some(0));
    let stdout_ver = String::from_utf8_lossy(&out_ver.stdout);
    assert!(stdout_ver.contains("agyp"));

    // 3. agyp task --version must print version and exit 0
    let out_task_ver = Command::new(bin)
        .args(["task", "--version"])
        .output()
        .unwrap();
    assert_eq!(out_task_ver.status.code(), Some(0));

    // 4. agyp prompt "some task prompt" --help must exit 0 and not be treated as task command
    let out_prompt_help = Command::new(bin)
        .args(["prompt", "some task prompt", "--help"])
        .output()
        .unwrap();
    assert_eq!(out_prompt_help.status.code(), Some(0));
    let stdout_prompt = String::from_utf8_lossy(&out_prompt_help.stdout);
    assert!(
        stdout_prompt.contains("agyp prompt") || stdout_prompt.contains("Usage:"),
        "Should show prompt help: {stdout_prompt}"
    );
}

#[test]
fn test_schema_always_serializes_exit_code_and_shapes() {
    // Finding 8: always serialize deprecated/backward-compatible exit_code, including null.
    // Test success, timeout, empty-output, and setup/argument error shapes.
    use commands::task::TaskJsonOutput;

    // 1. Timeout shape: exit_code MUST be explicitly present as null
    let timeout_out = TaskJsonOutput {
        profile: "dev".to_string(),
        worktree: "/wt".to_string(),
        canonical_worktree: Some("/wt".to_string()),
        status: "error".to_string(),
        failure_kind: Some("timeout".to_string()),
        agent_failure_kind: None,
        restore_error: None,
        exit_code: None,
        agent_exit_code: None,
        cli_exit_code: 1,
        timed_out: true,
        duration_ms: 500,
        stdout: "".to_string(),
        stderr: "timed out".to_string(),
    };
    let val_timeout = serde_json::to_value(&timeout_out).unwrap();
    assert!(
        val_timeout.as_object().unwrap().contains_key("exit_code"),
        "timeout JSON must serialize 'exit_code': null"
    );
    assert!(val_timeout["exit_code"].is_null());
    assert_eq!(val_timeout["agent_exit_code"], serde_json::Value::Null);
    assert_eq!(val_timeout["timed_out"], true);

    // 2. Setup error shape: exit_code MUST be explicitly present as null
    let setup_out = TaskJsonOutput {
        profile: "dev".to_string(),
        worktree: "/invalid".to_string(),
        canonical_worktree: None,
        status: "error".to_string(),
        failure_kind: Some("invalid_worktree".to_string()),
        agent_failure_kind: None,
        restore_error: None,
        exit_code: None,
        agent_exit_code: None,
        cli_exit_code: 1,
        timed_out: false,
        duration_ms: 10,
        stdout: "".to_string(),
        stderr: "bad worktree".to_string(),
    };
    let val_setup = serde_json::to_value(&setup_out).unwrap();
    assert!(
        val_setup.as_object().unwrap().contains_key("exit_code"),
        "setup error JSON must serialize 'exit_code': null"
    );
    assert!(val_setup["exit_code"].is_null());
    assert_eq!(val_setup["canonical_worktree"], serde_json::Value::Null);

    // 3. Success shape: exit_code is 0
    let success_out = TaskJsonOutput {
        profile: "dev".to_string(),
        worktree: "/wt".to_string(),
        canonical_worktree: Some("/wt".to_string()),
        status: "success".to_string(),
        failure_kind: None,
        agent_failure_kind: None,
        restore_error: None,
        exit_code: Some(0),
        agent_exit_code: Some(0),
        cli_exit_code: 0,
        timed_out: false,
        duration_ms: 1000,
        stdout: "success".to_string(),
        stderr: "".to_string(),
    };
    let val_success = serde_json::to_value(&success_out).unwrap();
    assert_eq!(val_success["exit_code"], 0);
    assert_eq!(val_success["agent_exit_code"], 0);
    assert_eq!(val_success["status"], "success");

    // 4. Empty output shape: exit_code is 0, agent_exit_code is 0, failure_kind is empty_output
    let empty_out = TaskJsonOutput {
        profile: "dev".to_string(),
        worktree: "/wt".to_string(),
        canonical_worktree: Some("/wt".to_string()),
        status: "error".to_string(),
        failure_kind: Some("empty_output".to_string()),
        agent_failure_kind: None,
        restore_error: None,
        exit_code: Some(0),
        agent_exit_code: Some(0),
        cli_exit_code: 1,
        timed_out: false,
        duration_ms: 800,
        stdout: "".to_string(),
        stderr: "".to_string(),
    };
    let val_empty = serde_json::to_value(&empty_out).unwrap();
    assert_eq!(val_empty["exit_code"], 0);
    assert_eq!(val_empty["agent_exit_code"], 0);
    assert_eq!(val_empty["failure_kind"], "empty_output");
    assert_eq!(val_empty["cli_exit_code"], 1);
}

// -------------------------------------------------------------
// TOCTOU & Classifier Hardening Tests
// -------------------------------------------------------------

#[test]
fn test_prompt_file_symlink_rejection_deterministic() {
    let temp = tempdir().unwrap();
    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, "valid prompt content").unwrap();

    let symlink_path = temp.path().join("symlink_prompt.txt");
    std::os::unix::fs::symlink(&target_file, &symlink_path).unwrap();

    // 1. Direct helper test
    let res = resolve_task_prompt_with_timeout(Some(&symlink_path), None);
    assert!(res.is_err(), "Symlink prompt file must be rejected!");
    match res {
        Err(ResolvePromptError::InvalidPrompt(msg)) => {
            assert!(
                msg.contains("cannot be a symlink"),
                "Error message should explicitly mention symlink rejection: {msg}"
            );
        }
        Err(ResolvePromptError::Timeout(_)) => {
            panic!("Unexpected timeout on symlink rejection");
        }
        Ok(_) => panic!("Symlink must not succeed"),
    }

    // 2. CLI integration test
    let (_wt_temp, _main, worktree) = create_test_git_worktree();
    let profiles_temp = tempdir().unwrap();
    let (_pdir, mock_keychain) = setup_test_profile(profiles_temp.path(), "dev", "dev@example.com");

    let temp_bin = tempdir().unwrap();
    let script = r#"#!/bin/sh
echo "should not run"
"#;
    let bin_dir = setup_test_bin_dir(temp_bin.path(), &mock_keychain, script);
    let path_env = format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = Command::new(bin)
        .env("PATH", &path_env)
        .env("AGYP_PROFILES_DIR", profiles_temp.path())
        .args([
            "task",
            "--profile",
            "dev",
            "--worktree",
            worktree.to_str().unwrap(),
            "--prompt-file",
            symlink_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();

    assert_ne!(output.status.code(), Some(0));
    let json_val: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert_eq!(json_val["status"], "error");
    assert_eq!(json_val["failure_kind"], "invalid_prompt");
    assert!(
        json_val["stderr"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("symlink"),
        "stderr should mention symlink: {}",
        json_val["stderr"]
    );
}

#[test]
fn test_prompt_file_fifo_rejection_deterministic() {
    let temp = tempdir().unwrap();
    let fifo_path = temp.path().join("fifo_prompt.txt");
    let status = Command::new("mkfifo").arg(&fifo_path).status().unwrap();
    assert!(status.success());

    // Direct helper test - must not hang!
    let (tx, rx) = std::sync::mpsc::channel();
    let fifo_clone = fifo_path.clone();
    std::thread::spawn(move || {
        let res = resolve_task_prompt_with_timeout(Some(&fifo_clone), None);
        let _ = tx.send(res);
    });

    let res = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("FIFO resolution hung instead of rejecting non-regular file immediately!");

    assert!(res.is_err());
    match res {
        Err(ResolvePromptError::InvalidPrompt(msg)) => {
            assert!(
                msg.contains("regular file") || msg.contains("FIFO"),
                "Error message should mention regular file or FIFO: {msg}"
            );
        }
        _ => panic!("Expected InvalidPrompt for FIFO"),
    }
}

#[test]
fn test_prompt_file_oversized_rejection_deterministic() {
    let temp = tempdir().unwrap();
    let large_file = temp.path().join("oversized_prompt.txt");
    // Write MAX_PROMPT_FILE_SIZE + 64 bytes
    let size = (MAX_PROMPT_FILE_SIZE + 64) as usize;
    let data = vec![b'A'; size];
    fs::write(&large_file, &data).unwrap();

    let res = resolve_task_prompt_with_timeout(Some(&large_file), None);
    assert!(res.is_err(), "Oversized prompt must be rejected!");
    match res {
        Err(ResolvePromptError::InvalidPrompt(msg)) => {
            assert!(
                msg.contains("exceeds maximum"),
                "Error message should mention exceeding maximum limit: {msg}"
            );
        }
        _ => panic!("Expected InvalidPrompt for oversized file"),
    }
}

#[test]
fn test_prompt_file_single_handle_proof() {
    // Helper-level proof that validation and read use the one opened handle:
    // We open a regular file, then delete and replace the file on disk before reading from the handle.
    // The handle must still successfully read the original content because it reads from the opened inode.
    let temp = tempdir().unwrap();
    let file_path = temp.path().join("original_prompt.txt");
    let original_content = "Safe original content from single opened handle";
    fs::write(&file_path, original_content).unwrap();

    // Open file handle
    let file = fs::File::open(&file_path).unwrap();

    // Now replace the path on disk with a completely different file
    fs::remove_file(&file_path).unwrap();
    fs::write(&file_path, "MALICIOUS REPLACEMENT CONTENT").unwrap();

    // read_prompt_from_opened_file uses the already-opened handle:
    let content = read_prompt_from_opened_file(file, &file_path)
        .expect("read_prompt_from_opened_file should succeed using the original opened handle");

    assert_eq!(
        content, original_content,
        "read_prompt_from_opened_file read replacement content instead of original handle content!"
    );
}

#[test]
fn test_prompt_file_replacement_race() {
    // Best-effort replacement race:
    // A background thread continuously toggles a path between a regular file and a symlink/FIFO
    // while multiple calls to read_prompt_file_safely are made.
    // Every call must either:
    // 1. Return the valid regular content.
    // 2. Return an InvalidPrompt error (e.g. symlink or FIFO rejected).
    // It must NEVER hang, panic, or follow a symlink to untrusted content.
    let temp = tempdir().unwrap();
    let race_file = temp.path().join("race_prompt.txt");
    let target_file = temp.path().join("target.txt");
    fs::write(&target_file, "symlink target").unwrap();
    fs::write(&race_file, "Valid race prompt content").unwrap();

    let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_clone = std::sync::Arc::clone(&stop_flag);
    let race_clone = race_file.clone();
    let target_clone = target_file.clone();

    let flipper = std::thread::spawn(move || {
        let mut toggle = false;
        while !stop_clone.load(std::sync::atomic::Ordering::Relaxed) {
            let _ = fs::remove_file(&race_clone);
            if toggle {
                let _ = fs::write(&race_clone, "Valid race prompt content");
            } else {
                let _ = std::os::unix::fs::symlink(&target_clone, &race_clone);
            }
            toggle = !toggle;
            std::thread::sleep(Duration::from_micros(200));
        }
    });

    for _ in 0..50 {
        let res = read_prompt_file_safely(&race_file, Some(Duration::from_millis(500)));
        match res {
            Ok(content) => {
                assert_eq!(content, "Valid race prompt content");
            }
            Err(ResolvePromptError::InvalidPrompt(msg)) => {
                assert!(
                    msg.contains("cannot be a symlink")
                        || msg.contains("does not exist")
                        || msg.contains("Failed to open prompt file"),
                    "Acceptable rejection error: {msg}"
                );
            }
            Err(ResolvePromptError::Timeout(d)) => {
                panic!(
                    "Prompt file resolution timed out after {:?}, deadline violated!",
                    d
                );
            }
        }
    }

    stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = flipper.join();
}

#[test]
fn test_prompt_file_deadline_timeout_enforcement() {
    let temp = tempdir().unwrap();
    let prompt_file = temp.path().join("prompt.txt");
    fs::write(&prompt_file, "Prompt content").unwrap();

    // A zero duration timeout must immediately return ResolvePromptError::Timeout
    let res = resolve_task_prompt_with_timeout(Some(&prompt_file), Some(Duration::from_millis(0)));
    match res {
        Err(ResolvePromptError::Timeout(d)) => {
            assert_eq!(d, Duration::from_millis(0));
        }
        other => panic!(
            "Expected Timeout error with 0 timeout duration, got {:?}",
            other
        ),
    }
}

#[test]
fn test_classifier_prose_vs_actual_errors_regressions() {
    // 1. Exit 0 + valid stdout + stderr prose must remain SUCCESS
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Implemented requested feature cleanly",
            "Error: permission denied handling was added\n"
        ),
        Ok(()),
        "Prose 'Error: permission denied handling was added' must remain success on exit 0!"
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Implemented requested feature cleanly",
            "Error: added RESOURCE_EXHAUSTED / 429 handling\n"
        ),
        Ok(()),
        "Prose 'Error: added RESOURCE_EXHAUSTED / 429 handling' must remain success on exit 0!"
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Updated network retry logic",
            "Added handling for RESOURCE_EXHAUSTED and error: 429 responses\n"
        ),
        Ok(())
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Updated auth error tests",
            "Test error: permission denied responses are now covered\n"
        ),
        Ok(())
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Refactored rate limiting",
            "Error: 429 retry backoff handler configured\n"
        ),
        Ok(())
    );

    // 2. Exit 0 + valid stdout + actual observed error in stderr must FAIL with specific category
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "Error: 429 RESOURCE_EXHAUSTED\n"
        ),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "RESOURCE_EXHAUSTED: rate limit\n"
        ),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "Individual quota reached for project 123456\n"
        ),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "Error: permission denied: access denied to /secret\n"
        ),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "Error: permission_denied: user rejected tool call\n"
        ),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "Permission denied by user\n"
        ),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(0),
            "Partial output",
            "tool call rejected: permission_denied\n"
        ),
        Err(TaskFailureKind::PermissionDenied)
    );

    // 3. Nonzero exit code + stderr signatures
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "Error: 429 RESOURCE_EXHAUSTED\n"),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "RESOURCE_EXHAUSTED: rate limit\n"),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "Individual quota reached\n"),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "Your quota reached limit\n"),
        Err(TaskFailureKind::QuotaExhausted)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "Error: permission denied: forbidden\n"),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "Error: permission_denied: forbidden\n"),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(1),
            "",
            "tool call rejected: permission_denied\n"
        ),
        Err(TaskFailureKind::PermissionDenied)
    );
    assert_eq!(
        classify_task_outcome(false, Some(1), "", "Permission denied by user\n"),
        Err(TaskFailureKind::PermissionDenied)
    );

    // 4. Nonzero exit code without special signatures -> AgentFailed (not QuotaExhausted or PermissionDenied)
    assert_eq!(
        classify_task_outcome(
            false,
            Some(1),
            "",
            "Error: permission denied handling was added\n"
        ),
        Err(TaskFailureKind::AgentFailed)
    );
    assert_eq!(
        classify_task_outcome(
            false,
            Some(1),
            "",
            "Error: added RESOURCE_EXHAUSTED / 429 handling\n"
        ),
        Err(TaskFailureKind::AgentFailed)
    );
}
