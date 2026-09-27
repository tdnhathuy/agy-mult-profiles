use std::fs;
use std::os::unix::fs::PermissionsExt;

use tempfile::tempdir;

#[path = "../src/security.rs"]
mod security;

#[test]
fn rejects_unsafe_components() {
    for value in ["", ".", "..", "../x", "a/b", "a\\b", "a\0b"] {
        assert!(
            security::validate_component(value, "profile").is_err(),
            "accepted {value:?}"
        );
    }
    assert!(security::validate_component("p1", "profile").is_ok());
    assert!(security::validate_component("work-account", "profile").is_ok());
}

#[test]
fn writes_private_files_and_directories() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path().join("private");
    security::secure_dir(&dir).unwrap();
    let file = dir.join("token.json");
    security::write_private_file(&file, b"secret").unwrap();

    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn process_lock_is_exclusive() {
    let tmp = tempdir().unwrap();
    let lock_path = tmp.path().join("agyp.lock");
    let first = security::ProcessLock::acquire(&lock_path).unwrap();
    let second_path = lock_path.clone();
    let waiter = std::thread::spawn(move || security::ProcessLock::acquire(&second_path).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(!waiter.is_finished());
    drop(first);
    drop(waiter.join().unwrap());
}

#[test]
fn interruption_flag_can_be_observed() {
    let tmp = tempdir().unwrap();
    let _lock = security::ProcessLock::acquire(&tmp.path().join("agyp.lock")).unwrap();
    security::ProcessLock::mark_interrupted_for_test();
    assert!(security::ProcessLock::interrupted());
}
