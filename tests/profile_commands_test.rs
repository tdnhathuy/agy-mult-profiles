use tempfile::tempdir;

#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
mod commands;
#[path = "../src/config.rs"]
mod config;

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
