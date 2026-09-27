#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

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
    profile_dir.join("keychain_token.json").is_file()
        || profile_dir.join("oauth_creds.json").is_file()
        || profile_dir.join("jetski-standalone-oauth-token").is_file()
}

pub fn extract_profile_email(profile_dir: &Path) -> Option<String> {
    // 1. Try keychain_token.json
    let keychain_file = profile_dir.join("keychain_token.json");
    if keychain_file.is_file() {
        if let Ok(content) = fs::read_to_string(&keychain_file) {
            if let Some(email) = crate::keychain::extract_email_from_token_string(&content) {
                return Some(email);
            }
        }
    }

    // 2. Try jetski-standalone-oauth-token
    let standalone_file = profile_dir.join("jetski-standalone-oauth-token");
    if standalone_file.is_file() {
        if let Ok(content) = fs::read_to_string(&standalone_file) {
            if let Some(email) = crate::keychain::extract_email_from_token_string(&content) {
                return Some(email);
            }
        }
    }

    // 3. Try oauth_creds.json
    let creds_file = profile_dir.join("oauth_creds.json");
    if creds_file.is_file() {
        if let Ok(content) = fs::read_to_string(&creds_file) {
            if let Some(email) = crate::keychain::extract_email_from_token_string(&content) {
                return Some(email);
            }
        }
    }

    // 4. Try google_accounts.json
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

pub struct CapturedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub fn run_command_captured_interruptibly(cmd: &mut Command) -> Result<CapturedOutput, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute agy: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let status = loop {
        if crate::security::ProcessLock::interrupted() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Interrupted while AGY was running; credentials were restored".to_string());
        }
        match child
            .try_wait()
            .map_err(|e| format!("Failed to wait for agy: {e}"))?
        {
            Some(status) => break status,
            None => thread::sleep(Duration::from_millis(100)),
        }
    };

    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    if let Some(mut pipe) = stdout {
        std::io::Read::read_to_end(&mut pipe, &mut stdout_bytes)
            .map_err(|e| format!("Failed to collect agy stdout: {e}"))?;
    }
    if let Some(mut pipe) = stderr {
        std::io::Read::read_to_end(&mut pipe, &mut stderr_bytes)
            .map_err(|e| format!("Failed to collect agy stderr: {e}"))?;
    }
    Ok(CapturedOutput {
        status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
    })
}

pub fn run_command_interruptibly(cmd: &mut Command) -> Result<ExitStatus, String> {
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute agy: {e}"))?;

    loop {
        if crate::security::ProcessLock::interrupted() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Interrupted while AGY was running; credentials were restored".to_string());
        }
        match child
            .try_wait()
            .map_err(|e| format!("Failed to wait for agy: {e}"))?
        {
            Some(status) => return Ok(status),
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}
