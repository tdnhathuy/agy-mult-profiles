#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn locate_agy_binary() -> Result<PathBuf, String> {
    // 1. Check PATH
    if let Ok(path_var) = env::var("PATH") {
        for p in env::split_paths(&path_var) {
            let candidate = p.join("agy");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    // 2. Check standard locations
    if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local/bin/agy");
        if local_bin.is_file() {
            return Ok(local_bin);
        }
    }

    let brew_bin = PathBuf::from("/opt/homebrew/bin/agy");
    if brew_bin.is_file() {
        return Ok(brew_bin);
    }

    let usr_local_bin = PathBuf::from("/usr/local/bin/agy");
    if usr_local_bin.is_file() {
        return Ok(usr_local_bin);
    }

    Err(
        "Could not find 'agy' binary in PATH or standard paths (~/.local/bin/agy, /opt/homebrew/bin/agy)."
            .to_string(),
    )
}

pub fn check_profile_auth(profile_dir: &Path) -> bool {
    let creds_file = profile_dir.join("oauth_creds.json");
    creds_file.is_file()
}

pub fn extract_profile_email(profile_dir: &Path) -> Option<String> {
    let accounts_file = profile_dir.join("google_accounts.json");
    if accounts_file.is_file() {
        if let Ok(content) = fs::read_to_string(&accounts_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(active) = val.get("active").and_then(|v| v.as_str()) {
                    return Some(active.to_string());
                }
            }
        }
    }
    None
}

pub fn build_agy_command(profile_dir: &Path, extra_args: &[String]) -> Result<Command, String> {
    let agy_bin = locate_agy_binary()?;
    let mut cmd = Command::new(agy_bin);
    cmd.arg(format!("--gemini_dir={}", profile_dir.display()));
    for arg in extra_args {
        cmd.arg(arg);
    }
    Ok(cmd)
}
