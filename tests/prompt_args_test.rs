use std::path::PathBuf;

#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
mod commands;
#[path = "../src/config.rs"]
mod config;

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
