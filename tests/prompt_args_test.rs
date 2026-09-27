use std::path::PathBuf;

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

use agy::build_agy_command;

#[test]
fn test_build_agy_command_arguments() {
    let profile_dir = PathBuf::from("/tmp/profiles/work");
    let extra_args = vec![
        "--dangerously-skip-permissions".to_string(),
        "-p".to_string(),
        "hello world".to_string(),
    ];
    let cmd = build_agy_command(&profile_dir, &extra_args).unwrap();
    let args: Vec<String> = cmd
        .get_args()
        .map(|s| s.to_string_lossy().to_string())
        .collect();

    assert!(args.contains(&"--gemini_dir=/tmp/profiles/work".to_string()));
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"hello world".to_string()));
}

#[test]
fn test_cmd_prompt_nonexistent_profile_fails() {
    let res = commands::prompt::cmd_prompt(
        Some("__nonexistent_profile_xyz__"),
        Some("test prompt"),
        None,
        &[],
        false,
        false,
    );
    assert_eq!(res, Err(1));
}

#[test]
fn test_cmd_exec_nonexistent_profile_fails() {
    let res = commands::exec::cmd_exec("__nonexistent_profile_xyz__", &[]);
    assert_eq!(res, Err(1));
}

#[test]
fn test_cmd_prompt_missing_profile_json_output() {
    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = std::process::Command::new(bin)
        .args([
            "prompt",
            "__nonexistent_profile_xyz__",
            "test prompt",
            "--json",
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout_str).unwrap();

    assert_eq!(val["profile"], "__nonexistent_profile_xyz__");
    assert!(val["email"].is_null());
    assert_eq!(val["status"], "error");
    assert_eq!(val["exit_code"], 1);
    assert_eq!(val["output"], "");
    assert!(val["error"].as_str().unwrap().contains("not found"));
}

#[test]
fn test_cmd_prompt_unauthenticated_profile_json_output() {
    let temp = tempfile::tempdir().unwrap();
    let profiles_dir = temp.path().join("profiles_store");
    let unauth_dir = profiles_dir.join("profiles").join("unauth_user");
    std::fs::create_dir_all(&unauth_dir).unwrap();

    let config_file = profiles_dir.join("profiles.json");
    let mut reg = config::ProfileRegistry::default();
    reg.add_profile(config::ProfileInfo {
        name: "unauth_user".to_string(),
        email: "unauth@example.com".to_string(),
        dir: unauth_dir,
        created_at: chrono::Utc::now(),
        last_used_at: None,
    })
    .unwrap();
    reg.save(&config_file).unwrap();

    let bin = env!("CARGO_BIN_EXE_agyp");
    let output = std::process::Command::new(bin)
        .env("AGYP_PROFILES_DIR", &profiles_dir)
        .args(["prompt", "unauth_user", "test prompt", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let val: serde_json::Value = serde_json::from_str(&stdout_str).unwrap();

    assert_eq!(val["profile"], "unauth_user");
    assert_eq!(val["email"], "unauth@example.com");
    assert_eq!(val["status"], "error");
    assert_eq!(val["exit_code"], 2);
    assert_eq!(val["output"], "");
    assert!(val["error"].as_str().unwrap().contains("not authenticated"));
}
