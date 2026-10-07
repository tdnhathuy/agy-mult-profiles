//! Per-profile `CODEX_HOME` layout.
//!
//! ```text
//! ~/.codex-profiles/
//!   profiles.json          registry (same format as agyp's)
//!   config.toml            shared Codex config, symlinked into every home
//!   homes/<name>/          CODEX_HOME of one profile
//!     auth.json            imported credentials (refresh disabled)
//!     config.toml -> ../../config.toml
//!     AGENTS.md, skills, rules -> ~/.codex/... (when present)
//! ```

use std::fs;
use std::path::{Path, PathBuf};

pub const HOMES_DIRNAME: &str = "homes";
pub const SHARED_CONFIG_FILENAME: &str = "config.toml";

/// Entries of the user's real `~/.codex` shared read-mostly with every profile.
const SHARED_CODEX_ENTRIES: &[&str] = &["AGENTS.md", "skills", "rules"];

const DEFAULT_SHARED_CONFIG: &str = r#"# Shared Codex config for every codexp profile.
# Each profile runs with CODEX_HOME=~/.codex-profiles/homes/<name>, which symlinks this file.
#
# codexp always enforces, regardless of this file:
#   model_provider = "openai"
#   cli_auth_credentials_store = "file"
#   forced_login_method = "chatgpt"
#
# Example:
# model = "gpt-5.5"
# model_reasoning_effort = "high"
"#;

pub fn profiles_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEXP_PROFILES_DIR") {
        return PathBuf::from(dir);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex-profiles")
}

pub fn registry_path() -> PathBuf {
    profiles_dir().join("profiles.json")
}

pub fn homes_root() -> PathBuf {
    profiles_dir().join(HOMES_DIRNAME)
}

pub fn home_dir(name: &str) -> PathBuf {
    homes_root().join(name)
}

/// The user's own `~/.codex`, resolved from the password database so a codexp run nested inside
/// another Codex session never mistakes a profile home for it.
fn real_codex_dir() -> Option<PathBuf> {
    crate::keychain::real_user_home()
        .ok()
        .map(|h| h.join(".codex"))
}

fn link_if_missing(target: &Path, link: &Path) -> Result<(), String> {
    if fs::symlink_metadata(link).is_ok() {
        return Ok(());
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).map_err(|e| {
        format!(
            "cannot link {} -> {}: {e}",
            link.display(),
            target.display()
        )
    })?;
    Ok(())
}

/// Creates the profile's CODEX_HOME (idempotent). Existing entries are left alone, so a config
/// Codex rewrote in place is never clobbered.
pub fn prepare_home(home: &Path) -> Result<(), String> {
    let root = home
        .parent()
        .and_then(Path::parent)
        .ok_or("invalid home path")?;
    crate::security::secure_dir(home)
        .map_err(|e| format!("cannot create {}: {e}", home.display()))?;

    let shared_config = root.join(SHARED_CONFIG_FILENAME);
    if !shared_config.exists() {
        fs::write(&shared_config, DEFAULT_SHARED_CONFIG)
            .map_err(|e| format!("cannot write {}: {e}", shared_config.display()))?;
    }
    link_if_missing(
        Path::new("../..").join(SHARED_CONFIG_FILENAME).as_path(),
        &home.join(SHARED_CONFIG_FILENAME),
    )?;

    if let Some(codex_dir) = real_codex_dir() {
        if codex_dir != home {
            for entry in SHARED_CODEX_ENTRIES {
                let target = codex_dir.join(entry);
                if target.exists() {
                    link_if_missing(&target, &home.join(entry))?;
                }
            }
        }
    }
    Ok(())
}

/// Writes the profile's auth.json atomically with 0600 permissions, so a Codex process starting
/// concurrently reads either the old or the new credentials, never a partial file.
pub fn write_auth(home: &Path, auth: &serde_json::Value) -> Result<(), String> {
    let path = home.join(super::auth::AUTH_FILENAME);
    crate::security::reject_symlink(&path).map_err(|e| e.to_string())?;
    let tmp = home.join(format!(
        ".{}.tmp.{}",
        super::auth::AUTH_FILENAME,
        std::process::id()
    ));
    let body = serde_json::to_vec_pretty(auth).map_err(|e| e.to_string())?;
    crate::security::write_private_file(&tmp, &body)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}
