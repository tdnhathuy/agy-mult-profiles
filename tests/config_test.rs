use std::path::PathBuf;
use tempfile::tempdir;

#[path = "../src/config.rs"]
mod config;
#[path = "../src/security.rs"]
mod security;
use config::{ProfileInfo, ProfileRegistry};

#[test]
fn test_registry_add_and_resolve_profile() {
    let mut reg = ProfileRegistry::default();
    let info = ProfileInfo {
        name: "work".to_string(),
        email: "user@company.com".to_string(),
        dir: PathBuf::from("/tmp/profiles/work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    };
    reg.add_profile(info).unwrap();

    // Direct resolution
    let resolved = reg.resolve_profile("work");
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().email, "user@company.com");

    // Alias resolution
    reg.set_alias("p1", "work").unwrap();
    let from_alias = reg.resolve_profile("p1");
    assert!(from_alias.is_some());
    assert_eq!(from_alias.unwrap().name, "work");
}

#[test]
fn test_registry_persistence() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("profiles.json");

    let mut reg = ProfileRegistry::default();
    reg.add_profile(ProfileInfo {
        name: "personal".to_string(),
        email: "user@gmail.com".to_string(),
        dir: dir.path().join("personal"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.set_default_profile("personal").unwrap();
    reg.save(&config_path).unwrap();

    let loaded = ProfileRegistry::load_or_default(&config_path).unwrap();
    assert_eq!(loaded.default_profile, Some("personal".to_string()));
    assert!(loaded.profiles.contains_key("personal"));
}

#[test]
fn test_remove_profile_and_cleanup_aliases() {
    let mut reg = ProfileRegistry::default();
    reg.add_profile(ProfileInfo {
        name: "work".to_string(),
        email: "work@example.com".to_string(),
        dir: PathBuf::from("/tmp/work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.set_alias("w", "work").unwrap();
    reg.set_default_profile("work").unwrap();

    assert!(reg.resolve_profile("w").is_some());
    assert_eq!(reg.default_profile.as_deref(), Some("work"));

    reg.remove_profile("work").unwrap();
    assert!(reg.resolve_profile("work").is_none());
    assert!(reg.resolve_profile("w").is_none());
    assert!(reg.default_profile.is_none());
}

#[test]
fn test_remove_alias() {
    let mut reg = ProfileRegistry::default();
    reg.add_profile(ProfileInfo {
        name: "work".to_string(),
        email: "work@example.com".to_string(),
        dir: PathBuf::from("/tmp/work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.set_alias("w", "work").unwrap();
    assert!(reg.remove_alias("w").is_ok());
    assert!(reg.remove_alias("w").is_err());
}

#[test]
fn test_set_default_profile_via_alias_and_invalid() {
    let mut reg = ProfileRegistry::default();
    reg.add_profile(ProfileInfo {
        name: "work".to_string(),
        email: "work@example.com".to_string(),
        dir: PathBuf::from("/tmp/work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.set_alias("w", "work").unwrap();

    reg.set_default_profile("w").unwrap();
    assert_eq!(reg.default_profile.as_deref(), Some("work"));

    assert!(reg.set_default_profile("nonexistent").is_err());
}

#[test]
fn test_paths_and_load_edge_cases() {
    let profiles_dir = ProfileRegistry::get_profiles_dir();
    assert!(profiles_dir.ends_with(".gemini-profiles"));

    let config_file = ProfileRegistry::get_config_file_path();
    assert!(config_file.ends_with(".gemini-profiles/profiles.json"));

    let dir = tempdir().unwrap();
    let nonexistent = dir.path().join("does_not_exist.json");
    let default_reg = ProfileRegistry::load_or_default(&nonexistent).unwrap();
    assert_eq!(default_reg.version, 1);
    assert!(default_reg.profiles.is_empty());

    let invalid_json = dir.path().join("invalid.json");
    std::fs::write(&invalid_json, "not valid json").unwrap();
    assert!(ProfileRegistry::load_or_default(&invalid_json).is_err());
}

#[test]
fn test_atomic_persistence_cleans_up_tmp() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("profiles.json");
    let reg = ProfileRegistry::default();
    reg.save(&config_path).unwrap();

    assert!(config_path.exists());
    let tmp_path = dir
        .path()
        .join(format!("profiles.json.tmp.{}", std::process::id()));
    assert!(!tmp_path.exists(), "Temp file should be renamed/removed");
}

#[test]
fn test_registry_update_does_not_lose_concurrent_changes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("profiles.json");

    let handles: Vec<_> = (0..16)
        .map(|i| {
            let path = path.clone();
            std::thread::spawn(move || {
                ProfileRegistry::update(&path, |reg| {
                    let loaded = reg.profiles.len();
                    // Widen the read-modify-write window so unsynchronized writers would collide.
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    reg.add_profile(ProfileInfo {
                        name: format!("p{i}"),
                        email: format!("p{i}@example.com"),
                        dir: PathBuf::from(format!("/tmp/p{i}")),
                        created_at: chrono::Utc::now(),
                        last_used_at: None,
                    })?;
                    Ok(loaded)
                })
                .unwrap()
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }

    let reg = ProfileRegistry::load_or_default(&path).unwrap();
    assert_eq!(reg.profiles.len(), 16);
}
