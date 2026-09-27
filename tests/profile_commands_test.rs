use tempfile::tempdir;

#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
mod commands;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/keychain.rs"]
mod keychain;

use config::{ProfileInfo, ProfileRegistry};

#[test]
fn test_default_profile_selection() {
    let dir = tempdir().unwrap();
    let _config_path = dir.path().join("profiles.json");
    let mut reg = ProfileRegistry::default();

    reg.add_profile(ProfileInfo {
        name: "p1".to_string(),
        email: "p1@example.com".to_string(),
        dir: dir.path().join("p1"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();

    reg.add_profile(ProfileInfo {
        name: "p2".to_string(),
        email: "p2@example.com".to_string(),
        dir: dir.path().join("p2"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();

    reg.set_default_profile("p2").unwrap();
    assert_eq!(reg.default_profile, Some("p2".to_string()));
}

#[test]
fn test_cmd_list_json_succeeds() {
    let res = commands::profile::cmd_list(true);
    assert!(res.is_ok());
}

#[test]
fn test_cmd_default_query_succeeds() {
    let res = commands::profile::cmd_default(None);
    assert!(res.is_ok());
}

#[test]
fn test_cmd_rm_nonexistent_profile_fails() {
    let res = commands::profile::cmd_rm("__nonexistent_profile_xyz__", true);
    assert_eq!(res, Err(1));
}

#[test]
fn test_cmd_login_import_current_warning_when_creds_missing() {
    let temp = tempfile::tempdir().unwrap();
    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = std::process::Command::new(bin)
        .env("HOME", temp.path())
        .env("AGYP_PROFILES_DIR", temp.path().join(".gemini-profiles"))
        .args(["login", "test_import_user", "--import-current"])
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Warning: No active credentials found in ~/.gemini/oauth_creds.json. Please run 'agyp login <profile>' without --import-current to authenticate."));
}
