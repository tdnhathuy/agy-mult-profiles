#![allow(dead_code)]

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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

/// Parses output from `security find-generic-password`.
/// Tri-state outcome:
/// - Ok(Some(token)) if exit code 0 and non-empty stdout
/// - Ok(None) ONLY if exit code 44 (errSecItemNotFound) or exact "item could not be found"
/// - Err(error_message) for all other exit codes, locked keychain, permission denied, or launch errors
pub fn parse_security_find_output(
    status_code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<Option<String>, String> {
    if status_code == Some(0) {
        let token = String::from_utf8_lossy(stdout).trim().to_string();
        if token.is_empty() {
            Ok(None)
        } else {
            Ok(Some(token))
        }
    } else if status_code == Some(44)
        || String::from_utf8_lossy(stderr).contains("could not be found")
    {
        Ok(None)
    } else {
        let err_detail = String::from_utf8_lossy(stderr).trim().to_string();
        let code_str = match status_code {
            Some(c) => format!("code {c}"),
            None => "terminated by signal".to_string(),
        };
        Err(format!(
            "Failed to read keychain token ({code_str}): {err_detail}"
        ))
    }
}

const KEYCHAIN_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const KEYCHAIN_RESTORE_TIMEOUT: Duration = Duration::from_secs(5);

fn run_security_command(args: &[&str], timeout: Duration) -> Result<Output, String> {
    let mut child = Command::new("security")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to invoke security tool: {e}"))?;
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to collect security tool output: {e}"));
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Keychain operation timed out".to_string());
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Failed while waiting for security tool: {e}"));
            }
        }
    }
}

fn duration_until(deadline: Instant) -> Result<Duration, String> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| "Keychain transaction deadline exceeded".to_string())
}

/// Reads the generic password token from macOS Keychain for service="gemini", account="antigravity".
/// Returns a tri-state Result:
/// - Ok(Some(token)) if present
/// - Ok(None) if absent (errSecItemNotFound)
/// - Err(...) on any command, locked, or permission error
pub fn read_system_keychain_token_with_timeout(
    timeout: Duration,
) -> Result<Option<String>, String> {
    let output = run_security_command(
        &[
            "find-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
        ],
        timeout,
    )?;

    parse_security_find_output(output.status.code(), &output.stdout, &output.stderr)
}

pub fn read_system_keychain_token() -> Result<Option<String>, String> {
    read_system_keychain_token_with_timeout(KEYCHAIN_COMMAND_TIMEOUT)
}

/// Writes or updates the generic password in macOS Keychain for service="gemini", account="antigravity".
pub fn write_system_keychain_token_with_timeout(
    token: &str,
    timeout: Duration,
) -> Result<(), String> {
    let output = run_security_command(
        &[
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
            "-w",
            token,
        ],
        timeout,
    )?;

    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "Failed to write keychain token: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub fn write_system_keychain_token(token: &str) -> Result<(), String> {
    write_system_keychain_token_with_timeout(token, KEYCHAIN_COMMAND_TIMEOUT)
}

/// Deletes the generic password from macOS Keychain for service="gemini", account="antigravity".
pub fn delete_system_keychain_token_with_timeout(timeout: Duration) -> Result<(), String> {
    let output = run_security_command(
        &[
            "delete-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            KEYCHAIN_ACCOUNT,
        ],
        timeout,
    )?;

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

pub fn delete_system_keychain_token() -> Result<(), String> {
    delete_system_keychain_token_with_timeout(KEYCHAIN_COMMAND_TIMEOUT)
}

pub trait KeychainBackend: Send + Sync {
    fn read_token(&self) -> Result<Option<String>, String>;
    fn write_token(&self, token: &str) -> Result<(), String>;
    fn delete_token(&self) -> Result<(), String>;

    fn read_token_until(&self, deadline: Instant) -> Result<Option<String>, String> {
        let _ = duration_until(deadline)?;
        self.read_token()
    }

    fn write_token_until(&self, token: &str, deadline: Instant) -> Result<(), String> {
        let _ = duration_until(deadline)?;
        self.write_token(token)
    }

    fn delete_token_until(&self, deadline: Instant) -> Result<(), String> {
        let _ = duration_until(deadline)?;
        self.delete_token()
    }
}

#[derive(Debug, Default, Clone)]
pub struct SystemKeychainBackend;

impl KeychainBackend for SystemKeychainBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        read_system_keychain_token()
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        write_system_keychain_token(token)
    }

    fn delete_token(&self) -> Result<(), String> {
        delete_system_keychain_token()
    }

    fn read_token_until(&self, deadline: Instant) -> Result<Option<String>, String> {
        read_system_keychain_token_with_timeout(duration_until(deadline)?)
    }

    fn write_token_until(&self, token: &str, deadline: Instant) -> Result<(), String> {
        write_system_keychain_token_with_timeout(token, duration_until(deadline)?)
    }

    fn delete_token_until(&self, deadline: Instant) -> Result<(), String> {
        delete_system_keychain_token_with_timeout(duration_until(deadline)?)
    }
}

#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct MockKeychainBackend {
    pub token: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    pub calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    pub fail_read: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub fail_write: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub fail_delete: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub read_error_message: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

#[cfg(test)]
impl MockKeychainBackend {
    pub fn new(initial_token: Option<String>) -> Self {
        Self {
            token: std::sync::Arc::new(std::sync::Mutex::new(initial_token)),
            calls: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            fail_read: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fail_write: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fail_delete: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            read_error_message: std::sync::Arc::new(std::sync::Mutex::new(None)),
        }
    }
}

#[cfg(test)]
impl KeychainBackend for MockKeychainBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        if let Ok(mut c) = self.calls.lock() {
            c.push("read".to_string());
        }
        if self.fail_read.load(std::sync::atomic::Ordering::SeqCst) {
            let msg = self
                .read_error_message
                .lock()
                .ok()
                .and_then(|m| m.clone())
                .unwrap_or_else(|| "Mock read error (keychain locked)".to_string());
            return Err(msg);
        }
        Ok(self.token.lock().unwrap().clone())
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        if let Ok(mut c) = self.calls.lock() {
            c.push(format!("write:{token}"));
        }
        if self.fail_write.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Mock write error (keychain write failed)".to_string());
        }
        *self.token.lock().unwrap() = Some(token.to_string());
        Ok(())
    }

    fn delete_token(&self) -> Result<(), String> {
        if let Ok(mut c) = self.calls.lock() {
            c.push("delete".to_string());
        }
        if self.fail_delete.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("Mock delete error".to_string());
        }
        *self.token.lock().unwrap() = None;
        Ok(())
    }
}

/// Saves the given token string into the profile directory under keychain_token.json.
pub fn save_token_to_profile(profile_dir: &Path, token: &str) -> io::Result<Option<String>> {
    crate::security::secure_dir(profile_dir)?;
    let token_path = profile_dir.join(TOKEN_FILENAME);
    crate::security::reject_symlink(&token_path)?;
    crate::security::write_private_file(&token_path, token.as_bytes())?;
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
pub struct KeychainGuard<B: KeychainBackend = SystemKeychainBackend> {
    backend: B,
    saved_system_token: Option<String>,
    target_profile_dir: Option<PathBuf>,
    persistence_attempted: bool,
}

impl KeychainGuard<SystemKeychainBackend> {
    pub fn new(target_profile_dir: &Path) -> Result<Self, String> {
        Self::new_with_backend(target_profile_dir, SystemKeychainBackend)
    }
}

impl<B: KeychainBackend> KeychainGuard<B> {
    pub fn new_with_backend_until(
        target_profile_dir: &Path,
        backend: B,
        deadline: Instant,
    ) -> Result<Self, String> {
        let profile_token = load_token_from_profile(target_profile_dir).ok_or_else(|| {
            format!(
                "No saved Keychain token in profile directory {}",
                target_profile_dir.display()
            )
        })?;

        let current_system_token = backend
            .read_token_until(deadline)
            .map_err(|e| format!("Failed to snapshot Keychain before activation: {e}"))?;

        let mut guard = Self {
            backend,
            saved_system_token: current_system_token,
            target_profile_dir: Some(target_profile_dir.to_path_buf()),
            persistence_attempted: false,
        };

        if let Err(activate_error) = guard.backend.write_token_until(&profile_token, deadline) {
            // No AGY session ran, so cleanup must never persist whichever credential is visible
            // after an uncertain activation. Any retry is restoration-only.
            guard.persistence_attempted = true;
            // The write may have reached Keychain before its process timed out or output
            // collection failed. Restore from the snapshot under a separate bounded budget.
            let restore_result =
                guard.restore_snapshot_until(Instant::now() + KEYCHAIN_RESTORE_TIMEOUT);
            if restore_result.is_ok() {
                guard.target_profile_dir = None;
            }
            return match restore_result {
                Ok(()) => Err(format!("Failed to activate profile Keychain token: {activate_error}")),
                Err(restore_error) => Err(format!(
                    "Failed to activate profile Keychain token: {activate_error}; also failed to restore prior Keychain state: {restore_error}"
                )),
            };
        }

        Ok(guard)
    }

    pub fn new_with_backend(target_profile_dir: &Path, backend: B) -> Result<Self, String> {
        Self::new_with_backend_until(
            target_profile_dir,
            backend,
            Instant::now() + KEYCHAIN_COMMAND_TIMEOUT,
        )
    }

    pub fn sync_and_restore_until(mut self, deadline: Instant) -> Result<(), String> {
        self.cleanup_until(Some(deadline))
    }

    /// Captures a refreshed token and restores the exact prior Keychain state.
    pub fn sync_and_restore(mut self) -> Result<(), String> {
        self.cleanup_until(None)
    }

    fn cleanup(&mut self) -> Result<(), String> {
        self.cleanup_until(None)
    }

    fn cleanup_until(&mut self, deadline: Option<Instant>) -> Result<(), String> {
        let Some(target_dir) = self.target_profile_dir.as_ref().cloned() else {
            return Ok(());
        };

        let persist_result = if self.persistence_attempted {
            Ok(())
        } else {
            self.persistence_attempted = true;
            match deadline {
                Some(deadline) => self.backend.read_token_until(deadline),
                None => self.backend.read_token(),
            }
            .and_then(|active_token| {
                active_token.ok_or_else(|| {
                    "AGY removed or failed to expose the active Keychain token".to_string()
                })
            })
            .and_then(|active_token| {
                save_token_to_profile(&target_dir, &active_token)
                    .map(|_| ())
                    .map_err(|e| format!("Failed to persist refreshed profile token: {e}"))
            })
        };

        // Restoration gets an independent bounded budget. A persistence read may consume the
        // command deadline, but it must never prevent restoration of the exact snapshot.
        let restore_result = self.restore_snapshot_until(Instant::now() + KEYCHAIN_RESTORE_TIMEOUT);
        if restore_result.is_ok() {
            self.target_profile_dir = None;
        }

        match (persist_result, restore_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(persist_error), Ok(())) => Err(persist_error),
            (Ok(()), Err(restore_error)) => {
                Err(format!("Failed to restore original Keychain state: {restore_error}"))
            }
            (Err(persist_error), Err(restore_error)) => Err(format!(
                "{persist_error}; additionally failed to restore original Keychain state: {restore_error}"
            )),
        }
    }

    fn restore_snapshot_until(&self, deadline: Instant) -> Result<(), String> {
        match self.saved_system_token.as_ref() {
            Some(previous) => self.backend.write_token_until(previous, deadline),
            None => self.backend.delete_token_until(deadline),
        }
    }
}

impl<B: KeychainBackend> Drop for KeychainGuard<B> {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("Warning: failed to restore Keychain state: {error}");
        }
    }
}
