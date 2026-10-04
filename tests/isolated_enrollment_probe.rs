use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::process::Command;
use tempfile::tempdir;

fn probe(root: &std::path::Path, name: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_agyp"))
        .env("AGYP_PROFILES_DIR", root)
        .args(["isolated-probe", name])
        .output()
        .unwrap()
}

#[test]
fn creates_private_empty_sandbox_without_running_agy_or_importing_credentials() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("profiles")).unwrap();
    fs::write(root.join("profiles/profile-1"), "sensitive original").unwrap();
    let result = probe(root, "profile-1");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let home = root.join("isolated-probe/profile-1/home");
    assert!(home.join(".gemini").is_dir());
    assert_eq!(
        fs::metadata(&home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(home.join(".gemini"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(fs::read_dir(home.join(".gemini")).unwrap().next().is_none());
    assert_eq!(
        fs::read_to_string(root.join("profiles/profile-1")).unwrap(),
        "sensitive original"
    );
    let text = String::from_utf8_lossy(&result.stdout);
    assert!(text.contains("not authenticated"));
    assert!(!text.contains("sensitive original"));
}

#[test]
fn refuses_symlink_and_nonempty_existing_sandbox() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir(root.join("isolated-probe")).unwrap();
    symlink(root.join("other"), root.join("isolated-probe/profile-1")).unwrap();
    assert!(!probe(root, "profile-1").status.success());
    fs::remove_file(root.join("isolated-probe/profile-1")).unwrap();
    fs::create_dir_all(root.join("isolated-probe/profile-1/home/.gemini")).unwrap();
    fs::write(
        root.join("isolated-probe/profile-1/home/.gemini/jetski-standalone-oauth-token"),
        "fake",
    )
    .unwrap();
    assert!(!probe(root, "profile-1").status.success());
}

#[test]
fn rejects_invalid_profile_and_symlinked_root() {
    let tmp = tempdir().unwrap();
    assert!(!probe(tmp.path(), "../victim").status.success());
    let root = tmp.path().join("root");
    let target = tmp.path().join("target");
    fs::create_dir(&target).unwrap();
    symlink(&target, &root).unwrap();
    assert!(!probe(&root, "profile-1").status.success());
}
