//! Mock-only feasibility probes. These DO NOT establish that agy 1.2.16 reads or
//! refreshes a file token, nor that it avoids macOS Keychain.
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::tempdir;

#[path = "../src/security.rs"]
mod security;

// To be supplied by the spike harness, never used by production agyp.
mod mock_isolation;
use mock_isolation::{prepare_mock_profile, run_mock};

fn mock_binary(root: &Path) -> PathBuf {
    let script = root.join("fake-agy");
    fs::write(&script, "#!/bin/sh\nset -eu\ncase \"$1\" in --gemini_dir=*) ;; *) exit 91 ;; esac\n[ \"$SSH_CONNECTION\" = '127.0.0.1 50000 127.0.0.1 22' ] || exit 92\n[ \"$SSH_CLIENT\" = '127.0.0.1 50000 22' ] || exit 93\n[ \"$HOME/.gemini\" = \"${1#--gemini_dir=}\" ] || exit 94\n[ -f \"$HOME/.gemini/jetski-standalone-oauth-token\" ] || exit 95\n[ \"$(cat \"$HOME/.gemini/jetski-standalone-oauth-token\")\" = \"$EXPECTED\" ] || exit 96\nprintf '%s\\n' \"$EXPECTED\" >> \"$HOME/.gemini/refresh-events\"\nprintf 'started\\n' >> \"$EVENTS\"\nsleep \"$PAUSE\"\nprintf 'ended\\n' >> \"$EVENTS\"\nprintf '%s\\n' \"$EXPECTED-refreshed\" > \"$HOME/.gemini/jetski-standalone-oauth-token\"\nexit \"$EXIT_CODE\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    script
}

#[test]
fn distinct_profiles_mock_overlap_and_refresh_only_own_token() {
    let tmp = tempdir().unwrap();
    let script = mock_binary(tmp.path());
    let a = prepare_mock_profile(tmp.path(), "a", "fake-a").unwrap();
    let b = prepare_mock_profile(tmp.path(), "b", "fake-b").unwrap();
    let events = tmp.path().join("events");
    let mut one = run_mock(&script, &a, "fake-a", &events, "1", "0.4").unwrap();
    let mut two = run_mock(&script, &b, "fake-b", &events, "0", "0.4").unwrap();
    assert!(!one.wait().unwrap().success());
    assert!(two.wait().unwrap().success());
    let timeline = fs::read_to_string(&events).unwrap();
    let lines: Vec<_> = timeline.lines().collect();
    assert_eq!(
        &lines[..2],
        &["started", "started"],
        "mock processes did not overlap: {timeline:?}"
    );
    assert_eq!(
        fs::read_to_string(a.join(".gemini/jetski-standalone-oauth-token")).unwrap(),
        "fake-a-refreshed\n"
    );
    assert_eq!(
        fs::read_to_string(b.join(".gemini/jetski-standalone-oauth-token")).unwrap(),
        "fake-b-refreshed\n"
    );
    assert_eq!(
        fs::read_to_string(a.join(".gemini/refresh-events")).unwrap(),
        "fake-a\n"
    );
    assert_eq!(
        fs::read_to_string(b.join(".gemini/refresh-events")).unwrap(),
        "fake-b\n"
    );
}

#[test]
fn same_profile_lock_serializes_and_times_out() {
    let tmp = tempdir().unwrap();
    let home = prepare_mock_profile(tmp.path(), "a", "fake-a").unwrap();
    let lock = home.join(".gemini/agyp.lock");
    let first = security::ProcessLock::acquire(&lock).unwrap();
    let contender = lock.clone();
    let waiter = std::thread::spawn(move || {
        security::ProcessLock::acquire_with_timeout(&contender, Duration::from_millis(120))
    });
    assert!(waiter.join().unwrap().is_err());
    drop(first);
    assert!(security::ProcessLock::acquire_with_timeout(&lock, Duration::from_millis(120)).is_ok());
}

#[test]
fn mock_exit_and_kill_release_child_without_refresh_on_kill() {
    let tmp = tempdir().unwrap();
    let home = prepare_mock_profile(tmp.path(), "a", "fake-a").unwrap();
    let script = mock_binary(tmp.path());
    let events = tmp.path().join("events");
    let mut child = run_mock(&script, &home, "fake-a", &events, "17", "0").unwrap();
    assert_eq!(child.wait().unwrap().code(), Some(17));
    security::write_private_file(
        &home.join(".gemini/jetski-standalone-oauth-token"),
        b"fake-a",
    )
    .unwrap();
    let mut hung = run_mock(&script, &home, "fake-a", &events, "0", "30").unwrap();
    let start = std::time::Instant::now();
    while !events.exists() || fs::read_to_string(&events).unwrap().lines().count() < 2 {
        assert!(start.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(10));
    }
    hung.kill().unwrap();
    assert!(!hung.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(home.join(".gemini/jetski-standalone-oauth-token")).unwrap(),
        "fake-a"
    );
}

#[test]
fn unsafe_paths_symlinks_and_wrong_profile_are_rejected_without_token_leaks() {
    let tmp = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let a = prepare_mock_profile(tmp.path(), "a", "fake-a").unwrap();
    symlink(outside.path(), tmp.path().join("escape")).unwrap();
    assert!(prepare_mock_profile(tmp.path(), "../bad", "fake").is_err());
    assert!(prepare_mock_profile(tmp.path(), "escape", "fake").is_err());
    assert_eq!(
        fs::metadata(&a).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(a.join(".gemini/jetski-standalone-oauth-token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let script = mock_binary(tmp.path());
    let events = tmp.path().join("events");
    let mut wrong = run_mock(&script, &a, "fake-b", &events, "0", "0").unwrap();
    assert_eq!(wrong.wait().unwrap().code(), Some(96));
    assert!(!events.exists());
    assert_eq!(
        fs::read_to_string(a.join(".gemini/jetski-standalone-oauth-token")).unwrap(),
        "fake-a"
    );
}
