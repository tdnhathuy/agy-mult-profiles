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

/// Budget for preparing a profile's isolated HOME and Keychain before AGY starts.
pub const PROFILE_KEYCHAIN_SETUP_TIMEOUT: Duration = Duration::from_secs(15);

/// The shared system Keychain must always be resolved against the real user home, even when
/// agyp itself runs inside an isolated profile HOME (e.g. invoked by an AGY agent).
fn run_security_command(args: &[&str], timeout: Duration) -> Result<Output, String> {
    let real_home = real_user_home().ok();
    run_security_command_in(args, timeout, real_home.as_deref())
}

/// Runs `security` with `HOME` overridden. macOS resolves the user Keychain search list and
/// default Keychain relative to `$HOME`, which is what makes per-profile Keychains possible.
fn run_security_command_in(
    args: &[&str],
    timeout: Duration,
    home: Option<&Path>,
) -> Result<Output, String> {
    let mut command = Command::new("security");
    if let Some(home) = home {
        command.env("HOME", home);
    }
    command.args(args);
    run_tool_with_timeout(&mut command, "security", timeout)
}

fn run_tool_with_timeout(
    command: &mut Command,
    tool: &str,
    timeout: Duration,
) -> Result<Output, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to invoke {tool} tool: {e}"))?;
    let start = Instant::now();

    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|e| format!("Failed to collect {tool} tool output: {e}"));
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
                return Err(format!("Failed while waiting for {tool} tool: {e}"));
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

    /// Whether AGY runs against this backend should use a per-profile isolated HOME and
    /// Keychain instead of swapping credentials through this backend directly.
    fn isolates_profiles(&self) -> bool {
        false
    }
}

#[derive(Debug, Default, Clone)]
pub struct SystemKeychainBackend;

impl KeychainBackend for SystemKeychainBackend {
    fn isolates_profiles(&self) -> bool {
        true
    }

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

pub const ISOLATED_HOMES_DIRNAME: &str = "homes";
pub const ISOLATED_KEYCHAIN_PASSWORD_FILENAME: &str = "keychain_password";
const SYSTEM_KEYCHAIN_PATH: &str = "/Library/Keychains/System.keychain";
/// Entries of the real HOME that the isolated HOME must own instead of symlinking.
const HOME_PRIVATE_ENTRIES: &[&str] = &["Library"];
/// `Keychains` holds the profile Keychain; `Preferences` holds its private search list.
const LIBRARY_PRIVATE_ENTRIES: &[&str] = &["Keychains", "Preferences"];

/// Resolves the account's home directory from the password database, ignoring `$HOME`.
pub fn real_user_home() -> Result<PathBuf, String> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut passwd,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || passwd.pw_dir.is_null() {
        return Err("Failed to resolve the real user home directory".to_string());
    }
    let dir = unsafe { CStr::from_ptr(passwd.pw_dir) };
    Ok(PathBuf::from(OsStr::from_bytes(dir.to_bytes())))
}

/// Isolated HOME for a profile: `<profiles_dir>/homes/<profile>`. It deliberately lives outside
/// the profile (`--gemini_dir`) directory so AGY never walks into the mirrored real HOME.
pub fn isolated_home_dir(profile_dir: &Path) -> Result<PathBuf, String> {
    let name = profile_dir
        .file_name()
        .ok_or_else(|| format!("Invalid profile directory: {}", profile_dir.display()))?;
    let profiles_dir = profile_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| format!("Invalid profile directory: {}", profile_dir.display()))?;
    Ok(profiles_dir.join(ISOLATED_HOMES_DIRNAME).join(name))
}

/// A profile's private HOME whose login Keychain holds only that profile's AGY credential.
#[derive(Debug, Clone)]
pub struct IsolatedProfileHome {
    pub home: PathBuf,
    pub keychain: PathBuf,
}

impl IsolatedProfileHome {
    pub fn backend(&self) -> IsolatedKeychainBackend {
        IsolatedKeychainBackend {
            home: self.home.clone(),
            keychain: self.keychain.clone(),
        }
    }

    /// The credential store AGY runs against with this HOME and `--gemini_dir=profile_dir`.
    pub fn store(&self, profile_dir: &Path) -> ProfileStore {
        ProfileStore::Keychain(self.backend(), TokenFileBackend::for_profile(profile_dir))
    }
}

fn private_dir(path: &Path) -> Result<(), String> {
    crate::security::reject_symlink(path).map_err(|e| e.to_string())?;
    crate::security::secure_dir(path)
        .map_err(|e| format!("Failed to create private directory {}: {e}", path.display()))
}

/// Symlinks every entry of `source` into `target` except `private` names, so tools AGY runs
/// (git, gh, npm, ssh, ...) still find the user's configuration. Entries already present in
/// `target` are left alone, and links whose real target disappeared are pruned.
fn mirror_entries(source: &Path, target: &Path, private: &[&str]) -> Result<(), String> {
    let entries =
        fs::read_dir(source).map_err(|e| format!("Failed to read {}: {e}", source.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read {}: {e}", source.display()))?;
        let name = entry.file_name();
        if private.iter().any(|p| name == **p) {
            continue;
        }
        let link = target.join(&name);
        match fs::symlink_metadata(&link) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match std::os::unix::fs::symlink(entry.path(), &link) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(e) => {
                        return Err(format!("Failed to link {}: {e}", link.display()));
                    }
                }
            }
            Err(e) => return Err(format!("Failed to inspect {}: {e}", link.display())),
        }
    }

    let entries =
        fs::read_dir(target).map_err(|e| format!("Failed to read {}: {e}", target.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(destination) = fs::read_link(&path) {
            if destination.starts_with(source) && fs::symlink_metadata(&destination).is_err() {
                let _ = fs::remove_file(&path);
            }
        }
    }
    Ok(())
}

fn load_or_create_keychain_password(profile_dir: &Path) -> Result<String, String> {
    let path = profile_dir.join(ISOLATED_KEYCHAIN_PASSWORD_FILENAME);
    crate::security::reject_symlink(&path).map_err(|e| e.to_string())?;
    if path.is_file() {
        let password = fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read profile Keychain password: {e}"))?;
        let password = password.trim().to_string();
        if !password.is_empty() {
            return Ok(password);
        }
    }

    let mut bytes = [0u8; 24];
    io::Read::read_exact(
        &mut fs::File::open("/dev/urandom")
            .map_err(|e| format!("Failed to open random source: {e}"))?,
        &mut bytes,
    )
    .map_err(|e| format!("Failed to generate profile Keychain password: {e}"))?;
    let password: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    crate::security::write_private_file(&path, password.as_bytes())
        .map_err(|e| format!("Failed to store profile Keychain password: {e}"))?;
    Ok(password)
}

fn path_str(path: &Path) -> Result<&str, String> {
    path.to_str()
        .ok_or_else(|| format!("Path is not valid UTF-8: {}", path.display()))
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (a.canonicalize(), b.canonicalize()),
            (Ok(ca), Ok(cb)) if ca == cb
        )
}

/// Parses the quoted, indented path list printed by `security list-keychains`.
fn parse_keychain_list(stdout: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(stdout)
        .lines()
        .map(|line| line.trim().trim_matches('"'))
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn run_isolated_security(
    home: &IsolatedProfileHome,
    args: &[&str],
    deadline: Instant,
    action: &str,
) -> Result<Output, String> {
    let output = run_security_command_in(args, duration_until(deadline)?, Some(&home.home))?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(format!(
            "Failed to {action}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Refuses to run AGY unless the isolated HOME resolves only to the profile Keychain (plus the
/// read-only System Keychain). Otherwise AGY could read or overwrite another account's token.
fn verify_isolated_keychain(home: &IsolatedProfileHome, deadline: Instant) -> Result<(), String> {
    let listed = run_isolated_security(home, &["list-keychains"], deadline, "list Keychains")?;
    for keychain in parse_keychain_list(&listed.stdout) {
        if !same_path(&keychain, &home.keychain) && keychain != Path::new(SYSTEM_KEYCHAIN_PATH) {
            return Err(format!(
                "Isolated Keychain search list unexpectedly includes {}",
                keychain.display()
            ));
        }
    }
    let default = run_isolated_security(
        home,
        &["default-keychain"],
        deadline,
        "read default Keychain",
    )?;
    for keychain in parse_keychain_list(&default.stdout) {
        if !same_path(&keychain, &home.keychain) {
            return Err(format!(
                "Isolated default Keychain is unexpectedly {}",
                keychain.display()
            ));
        }
    }
    Ok(())
}

/// AGY loads shared skills from `<profile>/skills`, not from the mirrored HOME. Link it to the
/// user's `~/.agents/skills` so every profile sees the same skills (agyp, orca-cli, ...).
/// A real directory the user created there is left alone; a stale link is repointed.
fn link_shared_skills(profile_dir: &Path, real_home: &Path) -> Result<(), String> {
    let source = real_home.join(".agents").join("skills");
    if !source.is_dir() {
        return Ok(());
    }
    let link = profile_dir.join("skills");
    match fs::symlink_metadata(&link) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if fs::read_link(&link)
                .map_err(|e| format!("Failed to read {}: {e}", link.display()))?
                == source
            {
                return Ok(());
            }
            fs::remove_file(&link)
                .map_err(|e| format!("Failed to replace {}: {e}", link.display()))?;
        }
        Ok(_) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Failed to inspect {}: {e}", link.display())),
    }
    match std::os::unix::fs::symlink(&source, &link) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(format!("Failed to link {}: {e}", link.display())),
    }
}

/// Creates (or refreshes) the profile's isolated HOME and unlocks its private Keychain.
/// Must be called while holding the profile lock.
pub fn prepare_isolated_home(
    profile_dir: &Path,
    deadline: Instant,
) -> Result<IsolatedProfileHome, String> {
    prepare_isolated_home_mirroring(profile_dir, &real_user_home()?, deadline)
}

/// Same as [`prepare_isolated_home`], mirroring `real_home` instead of the account's home.
pub fn prepare_isolated_home_mirroring(
    profile_dir: &Path,
    real_home: &Path,
    deadline: Instant,
) -> Result<IsolatedProfileHome, String> {
    let home_dir = isolated_home_dir(profile_dir)?;
    if let Some(homes_root) = home_dir.parent() {
        private_dir(homes_root)?;
    }
    private_dir(&home_dir)?;
    let library = home_dir.join("Library");
    private_dir(&library)?;
    private_dir(&library.join("Keychains"))?;
    private_dir(&library.join("Preferences"))?;

    mirror_entries(real_home, &home_dir, HOME_PRIVATE_ENTRIES)?;
    let real_library = real_home.join("Library");
    if real_library.is_dir() {
        mirror_entries(&real_library, &library, LIBRARY_PRIVATE_ENTRIES)?;
    }

    link_shared_skills(profile_dir, real_home)?;

    let home = IsolatedProfileHome {
        keychain: library.join("Keychains").join("login.keychain-db"),
        home: home_dir,
    };
    let keychain = path_str(&home.keychain)?;
    let password = load_or_create_keychain_password(profile_dir)?;

    if !home.keychain.exists() {
        run_isolated_security(
            &home,
            &["create-keychain", "-p", &password, keychain],
            deadline,
            "create profile Keychain",
        )?;
        // No lock-on-sleep and no idle timeout: unattended agents must not hit a locked Keychain.
        run_isolated_security(
            &home,
            &["set-keychain-settings", keychain],
            deadline,
            "configure profile Keychain",
        )?;
    }
    run_isolated_security(
        &home,
        &["unlock-keychain", "-p", &password, keychain],
        deadline,
        "unlock profile Keychain",
    )?;
    run_isolated_security(
        &home,
        &["list-keychains", "-d", "user", "-s", keychain],
        deadline,
        "set profile Keychain search list",
    )?;
    run_isolated_security(
        &home,
        &["default-keychain", "-d", "user", "-s", keychain],
        deadline,
        "set profile default Keychain",
    )?;
    verify_isolated_keychain(&home, deadline)?;
    Ok(home)
}

/// Reads and writes AGY's `gemini`/`antigravity` item in one profile's private Keychain.
/// Every call names the Keychain file explicitly so it can never touch another Keychain.
#[derive(Debug, Clone)]
pub struct IsolatedKeychainBackend {
    home: PathBuf,
    keychain: PathBuf,
}

impl IsolatedKeychainBackend {
    fn read_with_timeout(&self, timeout: Duration) -> Result<Option<String>, String> {
        let keychain = path_str(&self.keychain)?;
        let output = run_security_command_in(
            &[
                "find-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                KEYCHAIN_ACCOUNT,
                "-w",
                keychain,
            ],
            timeout,
            Some(&self.home),
        )?;
        parse_security_find_output(output.status.code(), &output.stdout, &output.stderr)
    }

    fn write_with_timeout(&self, token: &str, timeout: Duration) -> Result<(), String> {
        let keychain = path_str(&self.keychain)?;
        let output = run_security_command_in(
            &[
                "add-generic-password",
                "-U",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                KEYCHAIN_ACCOUNT,
                "-w",
                token,
                keychain,
            ],
            timeout,
            Some(&self.home),
        )?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Failed to write profile Keychain token: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    fn delete_with_timeout(&self, timeout: Duration) -> Result<(), String> {
        let keychain = path_str(&self.keychain)?;
        let output = run_security_command_in(
            &[
                "delete-generic-password",
                "-s",
                KEYCHAIN_SERVICE,
                "-a",
                KEYCHAIN_ACCOUNT,
                keychain,
            ],
            timeout,
            Some(&self.home),
        )?;
        if output.status.success()
            || String::from_utf8_lossy(&output.stderr).contains("could not be found")
        {
            Ok(())
        } else {
            Err(format!(
                "Failed to delete profile Keychain token: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
}

impl KeychainBackend for IsolatedKeychainBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        self.read_with_timeout(KEYCHAIN_COMMAND_TIMEOUT)
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        self.write_with_timeout(token, KEYCHAIN_COMMAND_TIMEOUT)
    }

    fn delete_token(&self) -> Result<(), String> {
        self.delete_with_timeout(KEYCHAIN_COMMAND_TIMEOUT)
    }

    fn read_token_until(&self, deadline: Instant) -> Result<Option<String>, String> {
        self.read_with_timeout(duration_until(deadline)?)
    }

    fn write_token_until(&self, token: &str, deadline: Instant) -> Result<(), String> {
        self.write_with_timeout(token, duration_until(deadline)?)
    }

    fn delete_token_until(&self, deadline: Instant) -> Result<(), String> {
        self.delete_with_timeout(duration_until(deadline)?)
    }
}

/// AGY's file-based token storage, relative to the profile (`--gemini_dir`) directory. Off macOS
/// AGY keeps its credential here whenever it does not use the desktop keyring.
pub const AGY_TOKEN_FILE: &str = "antigravity-cli/antigravity-oauth-token";

/// Reads and writes AGY's token file inside one profile directory. Each profile has its own
/// `--gemini_dir`, so the file is private to the profile without any HOME or keyring tricks.
#[derive(Debug, Clone)]
pub struct TokenFileBackend {
    path: PathBuf,
}

impl TokenFileBackend {
    pub fn for_profile(profile_dir: &Path) -> Self {
        Self {
            path: profile_dir.join(AGY_TOKEN_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// AGY's token file holds plain JSON, while a token saved from the macOS Keychain carries
/// go-keyring's base64 envelope. Unwrap it so profiles copied over from a Mac keep working.
fn plain_token_json(token: &str) -> String {
    let trimmed = token.trim();
    trimmed
        .strip_prefix("go-keyring-base64:")
        .and_then(|encoded| STANDARD.decode(encoded).ok())
        .and_then(|decoded| String::from_utf8(decoded).ok())
        .unwrap_or_else(|| trimmed.to_string())
}

impl KeychainBackend for TokenFileBackend {
    fn read_token(&self) -> Result<Option<String>, String> {
        crate::security::reject_symlink(&self.path).map_err(|e| e.to_string())?;
        match fs::read_to_string(&self.path) {
            Ok(content) => {
                let token = content.trim();
                Ok((!token.is_empty()).then(|| token.to_string()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Failed to read profile token file: {e}")),
        }
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        crate::security::reject_symlink(&self.path).map_err(|e| e.to_string())?;
        // Write-then-rename so a running AGY never reads a half-written token.
        let staged = self
            .path
            .with_extension(format!("agyp-tmp-{}", std::process::id()));
        crate::security::write_private_file(&staged, plain_token_json(token).as_bytes())
            .and_then(|()| fs::rename(&staged, &self.path))
            .map_err(|e| {
                let _ = fs::remove_file(&staged);
                format!("Failed to write profile token file: {e}")
            })
    }

    fn delete_token(&self) -> Result<(), String> {
        crate::security::reject_symlink(&self.path).map_err(|e| e.to_string())?;
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("Failed to delete profile token file: {e}")),
        }
    }
}

/// A profile's own credential store: its private Keychain on macOS, AGY's token file elsewhere.
///
/// On macOS, AGY 1.4+ also moves its login out of the Keychain into the profile's token file
/// and keeps it there from then on. Once that file exists it is the live credential, so it is
/// read first and kept in step with the Keychain; older AGY never creates it.
#[derive(Debug, Clone)]
pub enum ProfileStore {
    Keychain(IsolatedKeychainBackend, TokenFileBackend),
    TokenFile(TokenFileBackend),
}

impl KeychainBackend for ProfileStore {
    fn read_token(&self) -> Result<Option<String>, String> {
        match self {
            Self::Keychain(k, f) => match f.read_token()? {
                Some(token) => Ok(Some(token)),
                None => k.read_token(),
            },
            Self::TokenFile(b) => b.read_token(),
        }
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        match self {
            Self::Keychain(k, f) => {
                if f.path().exists() {
                    f.write_token(token)?;
                }
                k.write_token(token)
            }
            Self::TokenFile(b) => b.write_token(token),
        }
    }

    fn delete_token(&self) -> Result<(), String> {
        match self {
            Self::Keychain(k, f) => {
                f.delete_token()?;
                k.delete_token()
            }
            Self::TokenFile(b) => b.delete_token(),
        }
    }

    fn read_token_until(&self, deadline: Instant) -> Result<Option<String>, String> {
        match self {
            Self::Keychain(k, f) => match f.read_token()? {
                Some(token) => Ok(Some(token)),
                None => k.read_token_until(deadline),
            },
            Self::TokenFile(b) => b.read_token_until(deadline),
        }
    }

    fn write_token_until(&self, token: &str, deadline: Instant) -> Result<(), String> {
        match self {
            Self::Keychain(k, f) => {
                if f.path().exists() {
                    f.write_token(token)?;
                }
                k.write_token_until(token, deadline)
            }
            Self::TokenFile(b) => b.write_token_until(token, deadline),
        }
    }

    fn delete_token_until(&self, deadline: Instant) -> Result<(), String> {
        match self {
            Self::Keychain(k, f) => {
                f.delete_token()?;
                k.delete_token_until(deadline)
            }
            Self::TokenFile(b) => b.delete_token_until(deadline),
        }
    }
}

/// What one AGY run needs to use a profile's credential without seeing another account's.
#[derive(Debug, Clone)]
pub struct ProfileIsolation {
    /// `HOME` the AGY process must be started with; `None` where no private HOME is needed.
    pub home: Option<PathBuf>,
    pub store: ProfileStore,
}

/// Prepares the profile's private credential store for this platform. Must be called while
/// holding the profile lock.
///
/// macOS: an isolated HOME with its own login Keychain. Elsewhere: AGY's token file inside the
/// profile directory, with AGY pinned to file-based storage by [`apply_profile_home`].
pub fn prepare_profile_isolation(
    profile_dir: &Path,
    deadline: Instant,
) -> Result<ProfileIsolation, String> {
    if cfg!(target_os = "macos") {
        let isolated = prepare_isolated_home(profile_dir, deadline)?;
        return Ok(ProfileIsolation {
            store: isolated.store(profile_dir),
            home: Some(isolated.home),
        });
    }
    link_shared_skills(profile_dir, &real_user_home()?)?;
    Ok(ProfileIsolation {
        home: None,
        store: ProfileStore::TokenFile(TokenFileBackend::for_profile(profile_dir)),
    })
}

/// The Keychain a single AGY run uses: either the caller's backend as-is, or the profile's
/// own store when the backend opts into per-profile isolation.
#[derive(Debug, Clone)]
pub enum ProfileKeychain<B> {
    Shared(B),
    Isolated(ProfileStore),
}

impl<B: KeychainBackend> KeychainBackend for ProfileKeychain<B> {
    fn read_token(&self) -> Result<Option<String>, String> {
        match self {
            Self::Shared(b) => b.read_token(),
            Self::Isolated(b) => b.read_token(),
        }
    }

    fn write_token(&self, token: &str) -> Result<(), String> {
        match self {
            Self::Shared(b) => b.write_token(token),
            Self::Isolated(b) => b.write_token(token),
        }
    }

    fn delete_token(&self) -> Result<(), String> {
        match self {
            Self::Shared(b) => b.delete_token(),
            Self::Isolated(b) => b.delete_token(),
        }
    }

    fn read_token_until(&self, deadline: Instant) -> Result<Option<String>, String> {
        match self {
            Self::Shared(b) => b.read_token_until(deadline),
            Self::Isolated(b) => b.read_token_until(deadline),
        }
    }

    fn write_token_until(&self, token: &str, deadline: Instant) -> Result<(), String> {
        match self {
            Self::Shared(b) => b.write_token_until(token, deadline),
            Self::Isolated(b) => b.write_token_until(token, deadline),
        }
    }

    fn delete_token_until(&self, deadline: Instant) -> Result<(), String> {
        match self {
            Self::Shared(b) => b.delete_token_until(deadline),
            Self::Isolated(b) => b.delete_token_until(deadline),
        }
    }
}

/// Activates a profile's credential for one AGY run, taking ownership of the profile `lock`.
/// Returns the guard plus the `HOME` the AGY process must be started with (`None` when the
/// backend does not isolate profiles).
///
/// With a shared backend the lock is held until the guard is dropped, because the shared
/// Keychain can hold only one profile at a time. With an isolated Keychain the lock is released
/// once the Keychain is ready, so further runs of the same profile can start concurrently.
pub fn activate_profile<B: KeychainBackend>(
    profile_dir: &Path,
    backend: B,
    deadline: Instant,
    lock: crate::security::ProcessLock,
) -> Result<(KeychainGuard<ProfileKeychain<B>>, Option<PathBuf>), String> {
    if !backend.isolates_profiles() {
        let mut guard = KeychainGuard::new_with_backend_until(
            profile_dir,
            ProfileKeychain::Shared(backend),
            deadline,
        )?;
        guard._process_lock = Some(lock);
        return Ok((guard, None));
    }
    let isolation = prepare_profile_isolation(profile_dir, deadline)
        .map_err(|e| format!("Failed to prepare isolated profile Keychain: {e}"))?;
    let guard = activate_profile_store(profile_dir, isolation.store, deadline, lock)?;
    Ok((guard, isolation.home))
}

/// Seeds the isolated Keychain from `keychain_token.json` only when it holds no token: once
/// populated, it carries AGY's latest refresh and is shared by every run of the profile.
pub fn activate_isolated_profile<B: KeychainBackend>(
    profile_dir: &Path,
    isolated: &IsolatedProfileHome,
    deadline: Instant,
    lock: crate::security::ProcessLock,
) -> Result<KeychainGuard<ProfileKeychain<B>>, String> {
    activate_profile_store(profile_dir, isolated.store(profile_dir), deadline, lock)
}

/// Same as [`activate_isolated_profile`] for any per-profile store.
pub fn activate_profile_store<B: KeychainBackend>(
    profile_dir: &Path,
    backend: ProfileStore,
    deadline: Instant,
    lock: crate::security::ProcessLock,
) -> Result<KeychainGuard<ProfileKeychain<B>>, String> {
    let profile_token = load_token_from_profile(profile_dir).ok_or_else(|| {
        format!(
            "No saved Keychain token in profile directory {}",
            profile_dir.display()
        )
    })?;
    let current = backend
        .read_token_until(deadline)
        .map_err(|e| format!("Failed to read profile Keychain: {e}"))?;
    if current.is_none() {
        backend
            .write_token_until(&profile_token, deadline)
            .map_err(|e| format!("Failed to activate profile Keychain token: {e}"))?;
    }
    if crate::security::ProcessLock::interrupted() {
        return Err("Interrupted while activating the profile".to_string());
    }
    let session = IsolatedSession {
        lock_path: lock.path().to_path_buf(),
        // Installed before the lock (and its own handlers) is released, so signals stay routed.
        _signals: crate::security::SignalGuard::install()?,
    };
    drop(lock);
    Ok(KeychainGuard {
        backend: ProfileKeychain::Isolated(backend),
        saved_system_token: None,
        target_profile_dir: Some(profile_dir.to_path_buf()),
        persistence_attempted: false,
        isolated: Some(session),
        _process_lock: None,
    })
}

/// Drops any token in the profile's isolated Keychain so the next run re-seeds it from
/// `keychain_token.json`. Call after replacing that file outside an AGY run (e.g. an import).
pub fn invalidate_isolated_token(profile_dir: &Path, deadline: Instant) -> Result<(), String> {
    if cfg!(target_os = "macos") && !isolated_home_dir(profile_dir)?.exists() {
        return Ok(());
    }
    prepare_profile_isolation(profile_dir, deadline)?
        .store
        .delete_token_until(deadline)
}

/// Points an AGY command at the profile's isolated HOME, if any.
///
/// Off macOS it also pins AGY to its per-profile token file: AGY would otherwise prefer the
/// desktop keyring (Secret Service) when one is reachable, and that keyring is shared by every
/// profile. AGY skips the keyring for SSH sessions, so make every run look like one.
pub fn apply_profile_home(cmd: &mut Command, home: Option<&Path>) {
    if let Some(home) = home {
        cmd.env("HOME", home);
    }
    if !cfg!(target_os = "macos")
        && std::env::var_os("SSH_CONNECTION").is_none_or(|v| v.is_empty())
        && std::env::var_os("SSH_TTY").is_none_or(|v| v.is_empty())
    {
        cmd.env("SSH_TTY", "/dev/null");
    }
}

/// The credential plain `agy` (no profile) is currently logged in with, for `--import-current`.
pub fn read_current_agy_token() -> Result<Option<String>, String> {
    if cfg!(target_os = "macos") {
        return read_system_keychain_token();
    }
    if let Some(home) = dirs::home_dir() {
        let file = TokenFileBackend::for_profile(&home.join(".gemini"));
        if let Some(token) = file.read_token()? {
            return Ok(Some(token));
        }
    }
    // A desktop session keeps the login in the Secret Service keyring instead of the file.
    let mut lookup = Command::new("secret-tool");
    lookup.args([
        "lookup",
        "service",
        KEYCHAIN_SERVICE,
        "username",
        KEYCHAIN_ACCOUNT,
    ]);
    match run_tool_with_timeout(&mut lookup, "secret-tool", KEYCHAIN_COMMAND_TIMEOUT) {
        Ok(output) if output.status.success() => {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Ok((!token.is_empty()).then_some(token))
        }
        _ => Ok(None),
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
    /// Set for a profile's own isolated Keychain, which other runs of the same profile share.
    isolated: Option<IsolatedSession>,
    /// Held for the whole run when credentials are swapped through a shared Keychain.
    _process_lock: Option<crate::security::ProcessLock>,
}

/// An isolated Keychain is the profile's persistent credential store: nothing is restored on
/// exit, and the refreshed token is mirrored to `keychain_token.json` under the profile lock.
struct IsolatedSession {
    lock_path: PathBuf,
    _signals: crate::security::SignalGuard,
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
            isolated: None,
            _process_lock: None,
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
        if let Some(lock_path) = self.isolated.as_ref().map(|s| s.lock_path.clone()) {
            self.target_profile_dir = None;
            self.persistence_attempted = true;
            // The isolated Keychain stays authoritative, so a failed mirror loses nothing.
            if let Err(error) = self.mirror_isolated_token(&target_dir, &lock_path) {
                eprintln!("Warning: failed to mirror refreshed profile token: {error}");
            }
            return Ok(());
        }

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

    fn mirror_isolated_token(&self, target_dir: &Path, lock_path: &Path) -> Result<(), String> {
        let _lock =
            crate::security::FileLock::acquire_with_timeout(lock_path, KEYCHAIN_RESTORE_TIMEOUT)?;
        let token = self
            .backend
            .read_token_until(Instant::now() + KEYCHAIN_RESTORE_TIMEOUT)?
            .ok_or_else(|| "AGY removed the profile Keychain token".to_string())?;
        if load_token_from_profile(target_dir).as_deref() == Some(token.trim()) {
            return Ok(());
        }
        save_token_to_profile(target_dir, &token)
            .map(|_| ())
            .map_err(|e| format!("Failed to save profile token: {e}"))
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
