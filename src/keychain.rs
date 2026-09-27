#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::engine::general_purpose::{STANDARD, URL_SAFE};
use base64::Engine;
use serde_json::Value;

pub const KEYCHAIN_SERVICE: &str = "gemini";
pub const KEYCHAIN_ACCOUNT: &str = "antigravity";
pub const TOKEN_FILENAME: &str = "keychain_token.json";

/// Decodes the payload segment of a JWT string and parses it into JSON Value.
pub fn decode_jwt_payload(jwt: &str) -> Option<Value> {
    let parts: Vec<&str> = jwt.split('.').collect();
    if parts.len() < 2 {
        return None;
    }
    let payload_b64 = parts[1];
    let mut padded = payload_b64.to_string();
    let rem = padded.len() % 4;
    if rem > 0 {
        padded.push_str(&"=".repeat(4 - rem));
    }

    let bytes = URL_SAFE
        .decode(&padded)
        .or_else(|_| STANDARD.decode(&padded))
        .ok()?;

    serde_json::from_slice(&bytes).ok()
}

/// Extracts an email address from raw token string which may be:
/// 1. "go-keyring-base64:<base64-json>"
/// 2. Raw JSON containing "id_token" or "token.id_token" or "email"
pub fn extract_email_from_token_string(token_raw: &str) -> Option<String> {
    let trimmed = token_raw.trim();
    let json_str = if let Some(stripped) = trimmed.strip_prefix("go-keyring-base64:") {
        let decoded = STANDARD.decode(stripped).ok()?;
        String::from_utf8(decoded).ok()?
    } else {
        trimmed.to_string()
    };

    let val: Value = serde_json::from_str(&json_str).ok()?;

    // 1. Try top-level id_token
    if let Some(id_token) = val.get("id_token").and_then(|v| v.as_str()) {
        if let Some(payload) = decode_jwt_payload(id_token) {
            if let Some(email) = payload.get("email").and_then(|v| v.as_str()) {
                return Some(email.to_string());
            }
        }
    }

    // 2. Try nested token.id_token
    if let Some(id_token) = val
        .get("token")
        .and_then(|t| t.get("id_token"))
        .and_then(|v| v.as_str())
    {
        if let Some(payload) = decode_jwt_payload(id_token) {
            if let Some(email) = payload.get("email").and_then(|v| v.as_str()) {
                return Some(email.to_string());
            }
        }
    }

    // 3. Direct email field
    if let Some(email) = val.get("email").and_then(|v| v.as_str()) {
        return Some(email.to_string());
    }

    None
}

/// Reads the generic password token from macOS Keychain for service="gemini", account="antigravity".
pub fn read_system_keychain_token() -> Option<String> {
    let output = Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
        ])
        .output()
        .ok()?;

    if output.status.success() {
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !token.is_empty() {
            return Some(token);
        }
    }
    None
}

/// Writes or updates the generic password in macOS Keychain for service="gemini", account="antigravity".
pub fn write_system_keychain_token(token: &str) -> Result<(), String> {
    let output = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
            token,
        ])
        .output()
        .map_err(|e| format!("Failed to invoke security tool: {}", e))?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Failed to write keychain token: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Deletes the generic password from macOS Keychain for service="gemini", account="antigravity".
pub fn delete_system_keychain_token() -> Result<(), String> {
    let output = Command::new("security")
        .args([
            "delete-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
        ])
        .output()
        .map_err(|e| format!("Failed to invoke security tool: {}", e))?;

    // Even if it failed because item was not found, we treat as clean
    if output.status.success()
        || String::from_utf8_lossy(&output.stderr).contains("could not be found")
    {
        Ok(())
    } else {
        Err(format!(
            "Failed to delete keychain token: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Saves the given token string into the profile directory under keychain_token.json.
pub fn save_token_to_profile(profile_dir: &Path, token: &str) -> io::Result<Option<String>> {
    let token_path = profile_dir.join(TOKEN_FILENAME);
    fs::write(&token_path, token)?;
    let email = extract_email_from_token_string(token);
    Ok(email)
}

/// Loads the token string from the profile directory if it exists.
pub fn load_token_from_profile(profile_dir: &Path) -> Option<String> {
    let token_path = profile_dir.join(TOKEN_FILENAME);
    if token_path.is_file() {
        fs::read_to_string(&token_path)
            .ok()
            .map(|s| s.trim().to_string())
    } else {
        None
    }
}

/// Activates a profile's saved token into macOS Keychain.
pub fn activate_profile_token(profile_dir: &Path) -> Result<(), String> {
    if let Some(token) = load_token_from_profile(profile_dir) {
        write_system_keychain_token(&token)
    } else {
        Err(format!(
            "No saved keychain token found in profile directory {}",
            profile_dir.display()
        ))
    }
}

/// RAII Guard that activates a target profile's credentials into macOS Keychain,
/// captures any token refresh on exit, and restores the original Keychain state.
pub struct KeychainGuard {
    saved_system_token: Option<String>,
    target_profile_dir: Option<PathBuf>,
}

impl KeychainGuard {
    pub fn new(target_profile_dir: &Path) -> Result<Self, String> {
        let current_system_token = read_system_keychain_token();

        // If the profile has a saved token, activate it
        if let Some(profile_token) = load_token_from_profile(target_profile_dir) {
            write_system_keychain_token(&profile_token)?;
        }

        Ok(Self {
            saved_system_token: current_system_token,
            target_profile_dir: Some(target_profile_dir.to_path_buf()),
        })
    }

    /// Explicitly sync any updated Keychain token back to the profile directory
    /// before dropping or finishing.
    pub fn sync_and_restore(mut self) {
        self.cleanup();
    }

    fn cleanup(&mut self) {
        if let Some(target_dir) = self.target_profile_dir.take() {
            // If agy refreshed or wrote a token during execution, capture it
            if let Some(active_token) = read_system_keychain_token() {
                let _ = save_token_to_profile(&target_dir, &active_token);
            }
        }

        // Restore original system token
        if let Some(prev) = self.saved_system_token.take() {
            let _ = write_system_keychain_token(&prev);
        }
    }
}

impl Drop for KeychainGuard {
    fn drop(&mut self) {
        self.cleanup();
    }
}
