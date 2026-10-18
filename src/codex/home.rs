//! Per-profile `CODEX_HOME` layout.
//!
//! ```text
//! ~/.codex-profiles/
//!   profiles.json          registry (same format as agyp's)
//!   config.toml            shared Codex config, symlinked into every home
//!   shared/                session history every profile shares
//!     sessions/, archived_sessions/, thread-writer-locks/, history.jsonl, session_index.jsonl
//!     sqlite/              CODEX_SQLITE_HOME (thread index, history, logs, memories)
//!   homes/<name>/          CODEX_HOME of one profile
//!     auth.json            imported credentials (refresh disabled)
//!     config.toml -> ../../config.toml
//!     sessions, history.jsonl, ... -> ../../shared/...
//!     skills/              Claude's skills, one link each (see `claude::skills`)
//!     AGENTS.md, rules -> ~/.codex/... (when present)
//! ```
//!
//! A thread started under one profile can be resumed under any other: Codex sends the rollout
//! (including encrypted reasoning) back to the server, which accepts it from another account.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const HOMES_DIRNAME: &str = "homes";
pub const SHARED_CONFIG_FILENAME: &str = "config.toml";
pub const SHARED_DIRNAME: &str = "shared";

/// Entries of the user's real `~/.codex` shared read-mostly with every profile.
const SHARED_CODEX_ENTRIES: &[&str] = &["AGENTS.md", "rules"];

/// History directories and append-only logs every profile home links into `shared/`.
const SHARED_HISTORY_DIRS: &[&str] = &["sessions", "archived_sessions", "thread-writer-locks"];
const SHARED_HISTORY_FILES: &[&str] = &["history.jsonl", "session_index.jsonl"];

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

/// `shared/` of the profiles dir that owns `home` (`<profiles>/homes/<name>`).
fn shared_dir_for(home: &Path) -> Result<PathBuf, String> {
    home.parent()
        .and_then(Path::parent)
        .map(|root| root.join(SHARED_DIRNAME))
        .ok_or_else(|| "invalid home path".to_string())
}

/// `CODEX_SQLITE_HOME` for `home`: one set of databases for every profile, so the thread index
/// matches the shared rollouts.
pub fn sqlite_home_for(home: &Path) -> Result<PathBuf, String> {
    Ok(shared_dir_for(home)?.join("sqlite"))
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

/// Moves a profile's own history tree into the shared one. Rollouts are append-only and named by
/// thread id, so on a name clash the longer file is the more complete copy and wins.
fn merge_tree(src: &Path, dst: &Path) -> io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            merge_tree(&entry.path(), &to)?;
            continue;
        }
        let keep_ours = match fs::symlink_metadata(&to) {
            Ok(existing) => entry.metadata()?.len() > existing.len(),
            Err(_) => true,
        };
        if keep_ours {
            fs::rename(entry.path(), &to)?;
        }
    }
    fs::remove_dir_all(src)
}

/// Replaces the profile's own history entries with links into `shared/`, folding in whatever
/// the profile recorded before history was shared.
fn link_shared_history(home: &Path) -> Result<(), String> {
    let shared = shared_dir_for(home)?;
    let err = |path: &Path, e: io::Error| format!("cannot share {}: {e}", path.display());
    for name in SHARED_HISTORY_DIRS {
        let (own, target) = (home.join(name), shared.join(name));
        crate::security::secure_dir(&target).map_err(|e| err(&target, e))?;
        if fs::symlink_metadata(&own).is_ok_and(|m| m.is_dir()) {
            merge_tree(&own, &target).map_err(|e| err(&own, e))?;
        }
        link_if_missing(&Path::new("../..").join(SHARED_DIRNAME).join(name), &own)?;
    }
    for name in SHARED_HISTORY_FILES {
        let (own, target) = (home.join(name), shared.join(name));
        if fs::symlink_metadata(&own).is_ok_and(|m| m.is_file()) {
            let body = fs::read(&own).map_err(|e| err(&own, e))?;
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&target)
                .and_then(|mut f| f.write_all(&body))
                .map_err(|e| err(&target, e))?;
            fs::remove_file(&own).map_err(|e| err(&own, e))?;
        }
        link_if_missing(&Path::new("../..").join(SHARED_DIRNAME).join(name), &own)?;
    }
    let sqlite = sqlite_home_for(home)?;
    crate::security::secure_dir(&sqlite).map_err(|e| err(&sqlite, e))
}

/// Mirrors Claude's skills into the profile's own `skills/`. It stays a real directory because
/// Codex installs its `.system` skills there. Links that no longer match a Claude skill are
/// pruned; entries Codex created are left alone.
fn link_claude_skills(home: &Path) -> Result<(), String> {
    let dir = home.join("skills");
    let err = |e: io::Error| format!("cannot update {}: {e}", dir.display());
    // Earlier layouts linked the whole directory to ~/.codex/skills.
    if fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(&dir).map_err(err)?;
    }
    crate::security::secure_dir(&dir).map_err(err)?;
    let wanted = super::claude::skills();
    for entry in fs::read_dir(&dir).map_err(err)?.flatten() {
        let path = entry.path();
        let Ok(target) = fs::read_link(&path) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        if wanted.get(&name) != Some(&target) {
            fs::remove_file(&path).map_err(err)?;
        }
    }
    for (name, target) in &wanted {
        link_if_missing(target, &dir.join(name))?;
    }
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
    link_shared_history(home)?;
    link_claude_skills(home)?;

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
