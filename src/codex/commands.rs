use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Utc};
use serde::Serialize;
use serde_json::Value;

use super::auth::{inspect_auth, load_profile_auth, sanitize_for_import, AccountSummary};
use super::exec::{build_codex_command, classify_exec, enforced_config_args, parse_exec_events};
use super::home::{home_dir, homes_root, prepare_home, registry_path, write_auth};
use crate::commands::task::{
    parse_duration, resolve_task_prompt_with_timeout, validate_linked_worktree, ResolvePromptError,
    TaskFailureKind,
};
use crate::config::{ProfileInfo, ProfileRegistry};

const MAX_AUTH_FILE_SIZE: u64 = 256 * 1024;
const DEFAULT_TASK_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// A task is refused when its access token expires sooner than this: Codex cannot refresh it.
const MIN_TOKEN_VALIDITY: chrono::Duration = chrono::Duration::seconds(60);
const EXPIRING_SOON: chrono::Duration = chrono::Duration::hours(24);
const SANDBOX_MODES: &[&str] = &["read-only", "workspace-write", "danger-full-access"];

fn fail(msg: impl std::fmt::Display, code: i32) -> i32 {
    eprintln!("Error: {msg}");
    code
}

fn fmt_local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string()
}

fn fmt_remaining(until: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (until - now).num_seconds();
    if secs <= 0 {
        return "expired".to_string();
    }
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn reexport_hint(name: &str) -> String {
    format!("Export a fresh auth file from OmniRoute and run 'codexp import {name} --from <file>'.")
}

fn load_registry() -> Result<ProfileRegistry, i32> {
    ProfileRegistry::load_or_default(&registry_path()).map_err(|e| fail(e, 1))
}

fn read_export(from: &Path) -> Result<Value, String> {
    let meta = fs::metadata(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", from.display()));
    }
    if meta.len() > MAX_AUTH_FILE_SIZE {
        return Err(format!("{} is too large for an auth file", from.display()));
    }
    let raw =
        fs::read_to_string(from).map_err(|e| format!("cannot read {}: {e}", from.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("{} is not valid JSON: {e}", from.display()))
}

pub fn cmd_import(name: &str, from: &Path, force: bool, remove_source: bool) -> Result<(), i32> {
    crate::security::validate_component(name, "profile name").map_err(|e| fail(e, 1))?;
    let export = read_export(from).map_err(|e| fail(e, 1))?;
    let summary = inspect_auth(&export).map_err(|e| fail(format!("{}: {e}", from.display()), 1))?;
    let now = Utc::now();
    if summary.access_expires_at <= now + MIN_TOKEN_VALIDITY {
        return Err(fail(
            format!(
                "access token for {} expired at {}. Refresh the account in OmniRoute and export again.",
                summary.email,
                fmt_local(summary.access_expires_at)
            ),
            1,
        ));
    }
    let sanitized = sanitize_for_import(&export, now).map_err(|e| fail(e, 1))?;
    let home = home_dir(name);

    ProfileRegistry::update(&registry_path(), |reg| {
        if let Some(existing) = reg.profiles.get(name) {
            if let Ok((_, current)) = load_profile_auth(&existing.dir) {
                if current.account_id != summary.account_id && !force {
                    return Err(format!(
                        "profile '{name}' holds {} but this export is {}; pass --force to replace the account",
                        current.email, summary.email
                    ));
                }
            }
        }
        if let Some(other) = reg
            .profiles
            .values()
            .find(|p| p.name != name && p.email.eq_ignore_ascii_case(&summary.email))
        {
            if !force {
                return Err(format!(
                    "{} is already imported as profile '{}'; re-import into that profile or pass --force",
                    summary.email, other.name
                ));
            }
        }
        prepare_home(&home)?;
        write_auth(&home, &sanitized)?;
        let created_at = reg.profiles.get(name).map(|p| p.created_at).unwrap_or(now);
        let last_used_at = reg.profiles.get(name).and_then(|p| p.last_used_at);
        reg.add_profile(ProfileInfo {
            name: name.to_string(),
            email: summary.email.clone(),
            dir: home.clone(),
            created_at,
            last_used_at,
        })
    })
    .map_err(|e| fail(e, 1))?;

    if remove_source {
        if let Err(e) = fs::remove_file(from) {
            eprintln!(
                "Warning: imported, but could not delete {}: {e}",
                from.display()
            );
        }
    }

    println!(
        "Imported '{name}': {} ({}), access token valid until {} ({}).",
        summary.email,
        summary.plan.as_deref().unwrap_or("unknown plan"),
        fmt_local(summary.access_expires_at),
        fmt_remaining(summary.access_expires_at, now)
    );
    println!("Token refresh is disabled for codexp; re-import a fresh export before it expires.");
    Ok(())
}

#[derive(Serialize)]
struct ProfileRow {
    name: String,
    email: String,
    plan: Option<String>,
    aliases: Vec<String>,
    account_id: Option<String>,
    access_expires_at: Option<String>,
    expires_in_secs: Option<i64>,
    status: String,
    detail: Option<String>,
    codex_home: String,
}

fn profile_rows(reg: &ProfileRegistry, now: DateTime<Utc>) -> Vec<ProfileRow> {
    let mut names: Vec<&String> = reg.profiles.keys().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let p = &reg.profiles[name];
            let mut aliases: Vec<String> = reg
                .aliases
                .iter()
                .filter(|(_, target)| *target == name)
                .map(|(a, _)| a.clone())
                .collect();
            aliases.sort();
            let base = ProfileRow {
                name: name.clone(),
                email: p.email.clone(),
                plan: None,
                aliases,
                account_id: None,
                access_expires_at: None,
                expires_in_secs: None,
                status: "invalid".to_string(),
                detail: None,
                codex_home: p.dir.display().to_string(),
            };
            match load_profile_auth(&p.dir) {
                Ok((_, s)) => {
                    let left = s.access_expires_at - now;
                    let status = if left <= MIN_TOKEN_VALIDITY {
                        "expired"
                    } else if left <= EXPIRING_SOON {
                        "expiring"
                    } else {
                        "ready"
                    };
                    ProfileRow {
                        email: s.email,
                        plan: s.plan,
                        account_id: Some(s.account_id),
                        access_expires_at: Some(s.access_expires_at.to_rfc3339()),
                        expires_in_secs: Some(left.num_seconds().max(0)),
                        status: status.to_string(),
                        ..base
                    }
                }
                Err(e) => ProfileRow {
                    detail: Some(e),
                    ..base
                },
            }
        })
        .collect()
}

pub fn cmd_list(json_output: bool) -> Result<(), i32> {
    let reg = load_registry()?;
    let now = Utc::now();
    let rows = profile_rows(&reg, now);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&rows).unwrap());
        return Ok(());
    }
    if rows.is_empty() {
        println!("No codexp profiles. Import one with 'codexp import <name> --from <auth.json>'.");
        return Ok(());
    }
    println!(
        "{:<14} {:<32} {:<6} {:<10} {:<24} STATUS",
        "PROFILE", "EMAIL", "PLAN", "ALIASES", "TOKEN EXPIRES"
    );
    println!("{}", "-".repeat(96));
    for r in rows {
        let expires = match r.access_expires_at.as_deref() {
            Some(t) => {
                let t: DateTime<Utc> = t.parse().unwrap_or(now);
                format!("{} ({})", fmt_local(t), fmt_remaining(t, now))
            }
            None => "-".to_string(),
        };
        let aliases = if r.aliases.is_empty() {
            "-".to_string()
        } else {
            r.aliases.join(",")
        };
        println!(
            "{:<14} {:<32} {:<6} {:<10} {:<24} {}",
            r.name,
            r.email,
            r.plan.as_deref().unwrap_or("-"),
            aliases,
            expires,
            r.status
        );
    }
    Ok(())
}

pub fn cmd_rm(name: &str, yes: bool) -> Result<(), i32> {
    let reg = load_registry()?;
    let profile = reg
        .profiles
        .get(name)
        .cloned()
        .ok_or_else(|| fail(format!("profile '{name}' not found"), 1))?;
    if !yes {
        print!(
            "Remove codexp profile '{name}' ({}) and its CODEX_HOME? [y/N] ",
            profile.email
        );
        let _ = io::stdout().flush();
        let mut answer = String::new();
        let _ = io::stdin().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes") {
            println!("Aborted.");
            return Ok(());
        }
    }
    ProfileRegistry::update(&registry_path(), |reg| reg.remove_profile(name))
        .map_err(|e| fail(e, 1))?;
    if profile.dir.exists() {
        let dir = crate::security::validate_profile_dir_under(&profile.dir, &homes_root())
            .map_err(|e| fail(e, 1))?;
        // remove_dir_all removes the shared-entry symlinks without following them.
        fs::remove_dir_all(&dir)
            .map_err(|e| fail(format!("cannot remove {}: {e}", dir.display()), 1))?;
    }
    println!("Removed profile '{name}'.");
    Ok(())
}

pub fn cmd_alias_set(alias: &str, profile: &str) -> Result<(), i32> {
    crate::security::validate_component(alias, "alias").map_err(|e| fail(e, 1))?;
    ProfileRegistry::update(&registry_path(), |reg| {
        if reg.profiles.contains_key(alias) {
            return Err(format!("'{alias}' is already a profile name"));
        }
        reg.set_alias(alias, profile)
    })
    .map_err(|e| fail(e, 1))?;
    println!("Alias '{alias}' -> '{profile}'.");
    Ok(())
}

pub fn cmd_alias_rm(alias: &str) -> Result<(), i32> {
    ProfileRegistry::update(&registry_path(), |reg| reg.remove_alias(alias))
        .map_err(|e| fail(e, 1))?;
    println!("Removed alias '{alias}'.");
    Ok(())
}

pub fn cmd_alias_list(json_output: bool) -> Result<(), i32> {
    let reg = load_registry()?;
    let mut pairs: Vec<(&String, &String)> = reg.aliases.iter().collect();
    pairs.sort();
    if json_output {
        let map: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(a, p)| (a.to_string(), Value::from(p.as_str())))
            .collect();
        println!("{}", serde_json::to_string_pretty(&map).unwrap());
    } else {
        for (a, p) in pairs {
            println!("{a} -> {p}");
        }
    }
    Ok(())
}

/// Resolves a profile and its credentials, refusing when the imported access token is unusable.
fn resolve_ready_profile(
    name: &str,
) -> Result<(ProfileInfo, AccountSummary), (TaskFailureKind, String)> {
    let reg = ProfileRegistry::load_or_default(&registry_path())
        .map_err(|e| (TaskFailureKind::SetupError, e))?;
    let profile = reg.resolve_profile(name).cloned().ok_or_else(|| {
        (
            TaskFailureKind::InvalidProfile,
            format!("Profile or alias '{name}' not found."),
        )
    })?;
    let (_, summary) = load_profile_auth(&profile.dir).map_err(|e| {
        (
            TaskFailureKind::Unauthenticated,
            format!(
                "Profile '{}' has no usable credentials: {e}. {}",
                profile.name,
                reexport_hint(&profile.name)
            ),
        )
    })?;
    if summary.access_expires_at <= Utc::now() + MIN_TOKEN_VALIDITY {
        return Err((
            TaskFailureKind::Unauthenticated,
            format!(
                "Access token of profile '{}' ({}) expired at {}. {}",
                profile.name,
                summary.email,
                fmt_local(summary.access_expires_at),
                reexport_hint(&profile.name)
            ),
        ));
    }
    Ok((profile, summary))
}

/// JSON contract of `codexp task --json`.
#[derive(Debug, Serialize)]
pub struct CodexTaskOutput {
    pub profile: String,
    pub email: Option<String>,
    pub worktree: String,
    pub canonical_worktree: Option<String>,
    pub status: String,
    pub failure_kind: Option<String>,
    pub cli_exit_code: i32,
    pub agent_exit_code: Option<i32>,
    pub timed_out: bool,
    pub duration_ms: u64,
    /// Codex session id; pass it to `--resume` to continue the same conversation.
    pub thread_id: Option<String>,
    pub final_message: Option<String>,
    pub usage: Option<Value>,
    pub errors: Vec<String>,
    pub stderr: String,
}

pub struct TaskArgs<'a> {
    pub profile: &'a str,
    pub worktree: &'a Path,
    pub prompt_file: Option<&'a Path>,
    pub timeout: Option<&'a str>,
    pub model: Option<&'a str>,
    pub reasoning_effort: Option<&'a str>,
    pub sandbox: &'a str,
    pub resume: Option<&'a str>,
}

fn exit_code_for(kind: TaskFailureKind) -> i32 {
    match kind {
        TaskFailureKind::Unauthenticated => 2,
        _ => 1,
    }
}

pub fn run_task(args: &TaskArgs) -> CodexTaskOutput {
    let start = Instant::now();
    let mut out = CodexTaskOutput {
        profile: args.profile.to_string(),
        email: None,
        worktree: args.worktree.display().to_string(),
        canonical_worktree: None,
        status: "error".to_string(),
        failure_kind: None,
        cli_exit_code: 1,
        agent_exit_code: None,
        timed_out: false,
        duration_ms: 0,
        thread_id: None,
        final_message: None,
        usage: None,
        errors: Vec::new(),
        stderr: String::new(),
    };
    let setup_error = |mut out: CodexTaskOutput, kind: TaskFailureKind, msg: String| {
        out.failure_kind = Some(kind.as_str().to_string());
        out.cli_exit_code = exit_code_for(kind);
        out.timed_out = kind == TaskFailureKind::Timeout;
        out.stderr = msg;
        out.duration_ms = start.elapsed().as_millis() as u64;
        out
    };

    let timeout = match args.timeout.map(parse_duration).transpose() {
        Ok(t) => t.unwrap_or(DEFAULT_TASK_TIMEOUT),
        Err(e) => return setup_error(out, TaskFailureKind::InvalidDuration, e),
    };
    if !SANDBOX_MODES.contains(&args.sandbox) {
        let msg = format!(
            "Invalid sandbox '{}'; use one of {}",
            args.sandbox,
            SANDBOX_MODES.join(", ")
        );
        return setup_error(out, TaskFailureKind::SetupError, msg);
    }
    let (profile, summary) = match resolve_ready_profile(args.profile) {
        Ok(v) => v,
        Err((kind, msg)) => return setup_error(out, kind, msg),
    };
    out.profile = profile.name.clone();
    out.email = Some(summary.email.clone());

    let worktree = match validate_linked_worktree(args.worktree) {
        Ok(w) => w,
        Err(e) => return setup_error(out, TaskFailureKind::InvalidWorktree, e),
    };
    out.canonical_worktree = Some(worktree.display().to_string());

    let timed_out_msg = || format!("Task timed out after {timeout:?}");
    let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
        return setup_error(out, TaskFailureKind::Timeout, timed_out_msg());
    };
    let prompt = match resolve_task_prompt_with_timeout(args.prompt_file, Some(remaining)) {
        Ok(p) => p,
        Err(ResolvePromptError::Timeout(_)) => {
            return setup_error(out, TaskFailureKind::Timeout, timed_out_msg())
        }
        Err(ResolvePromptError::InvalidPrompt(e)) => {
            return setup_error(out, TaskFailureKind::InvalidPrompt, e)
        }
    };

    if let Err(e) = prepare_home(&profile.dir) {
        return setup_error(out, TaskFailureKind::SetupError, e);
    }

    let mut codex_args = vec!["exec".to_string()];
    if args.resume.is_some() {
        codex_args.push("resume".to_string());
    }
    codex_args.extend(enforced_config_args());
    codex_args.extend([
        "-c".to_string(),
        format!("sandbox_mode=\"{}\"", args.sandbox),
        "--json".to_string(),
    ]);
    if let Some(m) = args.model {
        codex_args.extend(["-m".to_string(), m.to_string()]);
    }
    if let Some(effort) = args.reasoning_effort {
        codex_args.extend([
            "-c".to_string(),
            format!("model_reasoning_effort=\"{effort}\""),
        ]);
    }
    codex_args.push("--".to_string());
    if let Some(id) = args.resume {
        codex_args.push(id.to_string());
    }
    codex_args.push(prompt);

    let mut cmd = match build_codex_command(&profile.dir, &codex_args) {
        Ok(c) => c,
        Err(e) => {
            let mut o = setup_error(out, TaskFailureKind::SetupError, e);
            o.cli_exit_code = 127;
            return o;
        }
    };
    cmd.current_dir(&worktree);

    let _ = ProfileRegistry::update(&registry_path(), |reg| {
        if let Some(p) = reg.profiles.get_mut(&profile.name) {
            p.last_used_at = Some(Utc::now());
        }
        Ok(())
    });

    let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
        return setup_error(out, TaskFailureKind::Timeout, timed_out_msg());
    };
    let _signals = match crate::security::SignalGuard::install() {
        Ok(g) => g,
        Err(e) => return setup_error(out, TaskFailureKind::SetupError, e),
    };
    let captured = match crate::agy::run_command_captured_interruptibly_with_timeout(
        &mut cmd,
        Some(remaining),
    ) {
        Ok(c) => c,
        Err(e) => return setup_error(out, TaskFailureKind::AgentFailed, e),
    };

    let stdout = String::from_utf8_lossy(&captured.stdout);
    let stderr = String::from_utf8_lossy(&captured.stderr).to_string();
    let events = parse_exec_events(&stdout);
    let exit_code = captured.status.and_then(|s| s.code());
    let classification = classify_exec(captured.timed_out, exit_code, &events, &stderr);

    out.timed_out = captured.timed_out;
    out.agent_exit_code = if captured.timed_out { None } else { exit_code };
    out.thread_id = events.thread_id;
    out.final_message = events.final_message;
    out.usage = events.usage;
    out.errors = events.errors;
    out.stderr = stderr;
    out.duration_ms = start.elapsed().as_millis() as u64;
    match classification {
        Ok(()) => {
            out.status = "success".to_string();
            out.cli_exit_code = 0;
        }
        Err(kind) => {
            out.failure_kind = Some(kind.as_str().to_string());
            out.cli_exit_code = match kind {
                TaskFailureKind::Unauthenticated => 2,
                _ => out.agent_exit_code.filter(|&c| c != 0).unwrap_or(1),
            };
            if kind == TaskFailureKind::Unauthenticated {
                out.errors.push(reexport_hint(&profile.name));
            }
        }
    }
    out
}

pub fn cmd_task(args: &TaskArgs, json_output: bool) -> Result<(), i32> {
    let out = run_task(args);
    if json_output {
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        if let Some(msg) = &out.final_message {
            println!("{msg}");
        }
        for e in &out.errors {
            eprintln!("{e}");
        }
        if out.status != "success" && !out.stderr.trim().is_empty() {
            eprintln!("{}", out.stderr.trim_end());
        }
        if let Some(id) = &out.thread_id {
            eprintln!("thread_id: {id}");
        }
        if let Some(kind) = &out.failure_kind {
            eprintln!(
                "Error [{kind}]: codexp task failed (exit {})",
                out.cli_exit_code
            );
        }
    }
    if out.cli_exit_code == 0 {
        Ok(())
    } else {
        Err(out.cli_exit_code)
    }
}

/// Runs the Codex CLI under a profile with inherited stdio (interactive TUI or any subcommand).
pub fn cmd_exec(profile_name: &str, args: &[String]) -> Result<(), i32> {
    let reg = load_registry()?;
    let profile = reg
        .resolve_profile(profile_name)
        .cloned()
        .ok_or_else(|| fail(format!("profile or alias '{profile_name}' not found"), 1))?;
    match load_profile_auth(&profile.dir) {
        Ok((_, s)) if s.access_expires_at <= Utc::now() + MIN_TOKEN_VALIDITY => {
            eprintln!(
                "Warning: access token of '{}' expired at {}. {}",
                profile.name,
                fmt_local(s.access_expires_at),
                reexport_hint(&profile.name)
            );
        }
        Ok(_) => {}
        Err(e) => eprintln!("Warning: {e}"),
    }
    prepare_home(&profile.dir).map_err(|e| fail(e, 1))?;

    let mut codex_args = enforced_config_args();
    codex_args.extend(args.iter().cloned());
    let mut cmd = build_codex_command(&profile.dir, &codex_args).map_err(|e| fail(e, 127))?;
    // Ctrl-C belongs to Codex (same process group); codexp only waits for it to exit.
    let _signals = crate::security::SignalGuard::install().map_err(|e| fail(e, 1))?;
    let status = cmd
        .status()
        .map_err(|e| fail(format!("failed to run codex: {e}"), 127))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(code) => Err(code),
        None => Err(1),
    }
}
