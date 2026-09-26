use std::fs;
use tempfile::tempdir;

#[path = "../src/agy.rs"]
mod agy;
use agy::{build_agy_command, check_profile_auth, extract_profile_email, locate_agy_binary};

#[test]
fn test_locate_agy_binary_exists() {
    let bin = locate_agy_binary();
    assert!(
        bin.is_ok(),
        "agy binary should be found on this system: {:?}",
        bin
    );
}

#[test]
fn test_profile_auth_detection() {
    let dir = tempdir().unwrap();
    assert!(!check_profile_auth(dir.path()));

    let creds_file = dir.path().join("oauth_creds.json");
    fs::write(&creds_file, r#"{"access_token":"dummy"}"#).unwrap();
    assert!(check_profile_auth(dir.path()));
}

#[test]
fn test_extract_profile_email() {
    let dir = tempdir().unwrap();
    let accounts_file = dir.path().join("google_accounts.json");
    fs::write(&accounts_file, r#"{"active":"test@example.com","old":[]}"#).unwrap();

    let email = extract_profile_email(dir.path());
    assert_eq!(email, Some("test@example.com".to_string()));

    // Missing file returns None
    let empty_dir = tempdir().unwrap();
    assert_eq!(extract_profile_email(empty_dir.path()), None);

    // Corrupted/non-matching json returns None
    let bad_accounts_file = empty_dir.path().join("google_accounts.json");
    fs::write(&bad_accounts_file, "not json").unwrap();
    assert_eq!(extract_profile_email(empty_dir.path()), None);

    // No "active" field
    fs::write(&bad_accounts_file, r#"{"inactive":"abc"}"#).unwrap();
    assert_eq!(extract_profile_email(empty_dir.path()), None);
}

#[test]
fn test_build_agy_command() {
    let dir = tempdir().unwrap();
    let args = vec!["status".to_string(), "--verbose".to_string()];
    let cmd = build_agy_command(dir.path(), &args);
    assert!(
        cmd.is_ok(),
        "build_agy_command should succeed: {:?}",
        cmd.err()
    );

    let cmd = cmd.unwrap();
    let program = cmd.get_program();
    assert!(program.to_string_lossy().ends_with("agy"));

    let cmd_args: Vec<_> = cmd
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert!(cmd_args.contains(&format!("--gemini_dir={}", dir.path().display())));
    assert!(cmd_args.contains(&"status".to_string()));
    assert!(cmd_args.contains(&"--verbose".to_string()));
}
