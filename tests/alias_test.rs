#[path = "../src/isolate.rs"]
mod isolate;
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

use config::{ProfileInfo, ProfileRegistry};

#[test]
fn test_alias_lifecycle() {
    let dir = tempdir().unwrap();
    let _config_path = dir.path().join("profiles.json");
    let mut reg = ProfileRegistry::default();

    reg.add_profile(ProfileInfo {
        name: "work".to_string(),
        email: "work@example.com".to_string(),
        dir: dir.path().join("work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();

    // Set alias
    reg.set_alias("w", "work").unwrap();
    assert_eq!(reg.aliases.get("w"), Some(&"work".to_string()));

    // Resolve through alias
    let resolved = reg.resolve_profile("w").unwrap();
    assert_eq!(resolved.name, "work");

    // Remove alias
    reg.remove_alias("w").unwrap();
    assert!(!reg.aliases.contains_key("w"));
}

#[test]
fn test_cmd_alias_list_json_succeeds() {
    let res = commands::alias::cmd_alias_list(true);
    assert!(res.is_ok());
}

#[test]
fn test_cmd_alias_list_text_succeeds() {
    let res = commands::alias::cmd_alias_list(false);
    assert!(res.is_ok());
}

#[test]
fn test_cmd_alias_set_nonexistent_profile_fails() {
    let res = commands::alias::cmd_alias_set("__alias_x__", "__nonexistent_profile_xyz__");
    assert_eq!(res, Err(1));
}

#[test]
fn test_cmd_alias_rm_nonexistent_alias_fails() {
    let res = commands::alias::cmd_alias_rm("__nonexistent_alias_xyz__");
    assert_eq!(res, Err(1));
}
