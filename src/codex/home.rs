//! Per-profile `CODEX_HOME` layout.
//!
//! ```text
//! ~/.codex-profiles/
//!   profiles.json          registry (same format as agyp's)
//!   config.toml            shared Codex config, symlinked into every home
//!   homes/<name>/          CODEX_HOME of one profile
//!     auth.json            imported credentials (refresh disabled)
//!     config.toml -> ../../config.toml
//!     sessions, archived_sessions, thread-writer-locks,
//!     history.jsonl, session_index.jsonl -> ~/.codex/...
//!     skills/              Claude's skills, one link each (see `claude::skills`)
//!     AGENTS.md, rules -> ~/.codex/... (when present)
//! ```
//!
//! Session history is the user's own `~/.codex`: every profile links its history into it and
//! runs with `CODEX_SQLITE_HOME=~/.codex`, so plain `codex` and every profile see one set of
//! threads. A thread started under one account can be resumed under any other: Codex sends the
//! rollout (including encrypted reasoning) back to the server, which accepts it from another
//! account. Earlier layouts kept that history in `~/.codex-profiles/shared/`; it is merged into
//! `~/.codex` on the next run.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const HOMES_DIRNAME: &str = "homes";
pub const SHARED_CONFIG_FILENAME: &str = "config.toml";
/// History dir of earlier layouts, merged into `~/.codex` and then renamed to `shared.migrated`.
const LEGACY_SHARED_DIRNAME: &str = "shared";

/// Per-thread tables of the legacy `shared/sqlite` databases merged into `~/.codex`. Every row is
/// keyed by a thread id (a UUID), so `INSERT OR IGNORE` never drops a different thread's row.
const LEGACY_THREAD_TABLES: &[(&str, &[&str])] = &[
    (
        "state_5",
        &[
            "threads",
            "thread_read_receipts",
            "thread_dynamic_tools",
            "thread_spawn_edges",
            "thread_attachments",
        ],
    ),
    (
        "thread_history_1",
        &[
            "thread_items",
            "thread_turns",
            "thread_history_projection_state",
            "thread_realtime_items",
        ],
    ),
    ("goals_1", &["thread_goals"]),
];
const THREAD_INDEX_DB: &str = "state_5.sqlite";

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

/// `CODEX_SQLITE_HOME` of every profile: plain `codex`'s own databases, so the thread index
/// matches the rollouts every profile writes into `~/.codex`.
pub fn sqlite_home() -> Result<PathBuf, String> {
    active_codex_dir()
}

/// The user's own `~/.codex`, resolved from the password database so a codexp run nested inside
/// another Codex session never mistakes a profile home for it.
fn real_codex_dir() -> Option<PathBuf> {
    crate::keychain::real_user_home()
        .ok()
        .map(|h| h.join(".codex"))
}

/// The CODEX_HOME plain `codex` uses, which `codexp active` writes credentials into.
/// `CODEXP_ACTIVE_CODEX_HOME` overrides it (tests); `CODEX_HOME` is ignored on purpose because
/// inside a codexp run it points at a profile home.
pub fn active_codex_dir() -> Result<PathBuf, String> {
    if let Ok(dir) = std::env::var("CODEXP_ACTIVE_CODEX_HOME") {
        return Ok(PathBuf::from(dir));
    }
    real_codex_dir().ok_or_else(|| "cannot resolve the user's home directory".to_string())
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

/// Points `link` at `target`, replacing a link that points elsewhere (an earlier layout).
fn relink(target: &Path, link: &Path) -> Result<(), String> {
    if fs::read_link(link).is_ok_and(|current| current != target) {
        fs::remove_file(link).map_err(|e| format!("cannot replace {}: {e}", link.display()))?;
    }
    link_if_missing(target, link)
}

/// Appends `src` to `dst` and removes `src`.
fn append_into(src: &Path, dst: &Path) -> io::Result<()> {
    let body = fs::read(src)?;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dst)
        .and_then(|mut f| f.write_all(&body))?;
    fs::remove_file(src)
}

/// Replaces the profile's own history entries with links into `~/.codex`, folding in whatever
/// the profile or an earlier layout recorded on its own.
fn link_shared_history(home: &Path) -> Result<(), String> {
    let codex = active_codex_dir()?;
    let err = |path: &Path, e: io::Error| format!("cannot share {}: {e}", path.display());
    crate::security::secure_dir(&codex).map_err(|e| err(&codex, e))?;
    if let Some(root) = home.parent().and_then(Path::parent) {
        migrate_legacy_shared(root, &codex)?;
    }
    for name in SHARED_HISTORY_DIRS {
        let (own, target) = (home.join(name), codex.join(name));
        crate::security::secure_dir(&target).map_err(|e| err(&target, e))?;
        if fs::symlink_metadata(&own).is_ok_and(|m| m.is_dir()) {
            merge_tree(&own, &target).map_err(|e| err(&own, e))?;
        }
        relink(&target, &own)?;
    }
    for name in SHARED_HISTORY_FILES {
        let (own, target) = (home.join(name), codex.join(name));
        if fs::symlink_metadata(&own).is_ok_and(|m| m.is_file()) {
            append_into(&own, &target).map_err(|e| err(&own, e))?;
        }
        relink(&target, &own)?;
    }
    Ok(())
}

/// Moves the history of the earlier `<profiles>/shared/` layout into `~/.codex` once. The
/// rollouts and logs move as files; the thread index rows are copied with `sqlite3`, and the
/// old databases stay in `shared.migrated/` as a backup (or for a manual merge if that failed).
fn migrate_legacy_shared(root: &Path, codex: &Path) -> Result<(), String> {
    let legacy = root.join(LEGACY_SHARED_DIRNAME);
    if fs::symlink_metadata(&legacy).is_err() {
        return Ok(());
    }
    // Claiming the directory by renaming it lets exactly one concurrent run migrate it.
    let claimed = root.join(format!(
        "{LEGACY_SHARED_DIRNAME}.migrating.{}",
        std::process::id()
    ));
    if fs::rename(&legacy, &claimed).is_err() {
        return Ok(());
    }
    let err = |path: &Path, e: io::Error| format!("cannot migrate {}: {e}", path.display());
    for name in SHARED_HISTORY_DIRS {
        let src = claimed.join(name);
        if src.is_dir() {
            merge_tree(&src, &codex.join(name)).map_err(|e| err(&src, e))?;
        }
    }
    for name in SHARED_HISTORY_FILES {
        let src = claimed.join(name);
        if src.is_file() {
            append_into(&src, &codex.join(name)).map_err(|e| err(&src, e))?;
        }
    }
    let index = claimed.join("sqlite");
    let merged = !index.is_dir() || {
        let result = merge_legacy_index(&index, codex, root);
        if let Err(e) = &result {
            eprintln!(
                "Warning: codexp threads were not added to the thread index in {}: {e}",
                codex.display()
            );
        }
        result.is_ok()
    };
    let backup = root.join(format!("{LEGACY_SHARED_DIRNAME}.migrated"));
    let kept = if fs::rename(&claimed, &backup).is_ok() {
        backup
    } else {
        claimed
    };
    eprintln!(
        "codexp: moved profile history into {}{}",
        codex.display(),
        if merged {
            format!(
                "; old databases kept in {} (safe to delete)",
                kept.display()
            )
        } else {
            format!("; old databases kept in {}", kept.display())
        }
    );
    Ok(())
}

fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn sqlite3(db: &Path, sql: &str) -> Result<String, String> {
    let out = Command::new("sqlite3")
        .arg("-bail")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|e| format!("cannot run sqlite3: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// SQL that points rollout paths recorded under `prefix` (Codex records them under the
/// CODEX_HOME it ran with) at the same file in `codex`, so they outlive the profile home.
fn repoint_rollouts_sql(db: &str, prefix: &Path, codex: &Path) -> String {
    let prefix = format!("{}/", prefix.display());
    let codex = codex.display().to_string();
    // archived_sessions first: "/sessions/" does not match inside "/archived_sessions/".
    ["/archived_sessions/", "/sessions/"]
        .iter()
        .map(|sub| {
            format!(
                "UPDATE {db}.threads SET rollout_path = {codex} || substr(rollout_path, instr(rollout_path, {sub})) \
                 WHERE substr(rollout_path, 1, {len}) = {prefix} AND instr(rollout_path, {sub}) > 0;",
                codex = sql_str(&codex),
                sub = sql_str(sub),
                len = prefix.chars().count(),
                prefix = sql_str(&prefix),
            )
        })
        .collect()
}

/// Repoints thread rollout paths under `prefix` (e.g. a profile home about to be removed) at
/// `~/.codex`. Best effort: without `sqlite3` or an index the paths are left as they are.
pub fn repoint_thread_rollouts(prefix: &Path) -> Result<(), String> {
    let codex = active_codex_dir()?;
    let index = codex.join(THREAD_INDEX_DB);
    if !index.is_file() {
        return Ok(());
    }
    sqlite3(&index, &repoint_rollouts_sql("main", prefix, &codex)).map(|_| ())
}

/// Copies the per-thread rows of the legacy databases in `index` into `codex`'s, in one
/// transaction. Refuses when a schema version differs, since rows are copied column by column.
fn merge_legacy_index(index: &Path, codex: &Path, root: &Path) -> Result<(), String> {
    let version = |db: &Path| sqlite3(db, "SELECT max(version) FROM _sqlx_migrations;");
    let (mut attach, mut copy) = (String::new(), String::new());
    for (i, (db, tables)) in LEGACY_THREAD_TABLES.iter().enumerate() {
        let (old, new) = (
            index.join(format!("{db}.sqlite")),
            codex.join(format!("{db}.sqlite")),
        );
        if !old.is_file() {
            continue;
        }
        if !new.is_file() {
            return Err(format!(
                "{} does not exist; run plain codex once, then merge {} by hand",
                new.display(),
                old.display()
            ));
        }
        if version(&old)? != version(&new)? {
            return Err(format!(
                "{db} schema of {} differs from {}; update both to the same Codex version",
                old.display(),
                new.display()
            ));
        }
        attach += &format!(
            "ATTACH {} AS o{i}; ATTACH {} AS n{i};",
            sql_str(&old.display().to_string()),
            sql_str(&new.display().to_string())
        );
        for table in *tables {
            copy += &format!("INSERT OR IGNORE INTO n{i}.{table} SELECT * FROM o{i}.{table};");
        }
        if *db == "state_5" {
            copy += &repoint_rollouts_sql(&format!("n{i}"), root, codex);
            // Sections and projects are per-database; drop references the new one lacks.
            copy += &format!(
                "UPDATE n{i}.threads SET thread_section_id = NULL, section_position = NULL, section_entered_at_ms = NULL \
                 WHERE thread_section_id IS NOT NULL AND thread_section_id NOT IN (SELECT id FROM n{i}.thread_sections);\
                 UPDATE n{i}.threads SET project_id = NULL \
                 WHERE project_id IS NOT NULL AND project_id NOT IN (SELECT id FROM n{i}.projects);"
            );
        }
    }
    if copy.is_empty() {
        return Ok(());
    }
    sqlite3(
        Path::new(":memory:"),
        &format!("{attach} BEGIN; {copy} COMMIT;"),
    )
    .map(|_| ())
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
