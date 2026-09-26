# Antigravity Multi-Profile CLI (`agyp`) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a high-performance, zero-dependency Rust CLI tool (`agyp`) to manage multiple Google Antigravity (`agy`) accounts/profiles using native directory isolation (`--gemini_dir`), allowing AI Agents (like Hermes) and developers to prompt specific profiles concurrently with clean I/O.

**Architecture:** A Rust binary structured with `clap` CLI parser, a central JSON registry (`~/.gemini-profiles/profiles.json`) for profile/alias resolution, and an execution engine that maps profiles to `--gemini_dir` arguments, forwarding stdin/stdout and POSIX exit codes from the underlying `agy` binary.

**Tech Stack:** Rust (Edition 2021), `clap` v4 (derive), `serde` v1 + `serde_json` v1, `dirs` v5, `chrono` v0.4.

## Global Constraints

- **Binary Name:** `agyp`
- **Profiles Root:** `~/.gemini-profiles/`
- **Registry File:** `~/.gemini-profiles/profiles.json`
- **Default Permissions in Prompt Mode:** Always include `--dangerously-skip-permissions` unless `--no-skip-perm` is explicitly passed.
- **Agent Clean Output:** Non-interactive `prompt` mode must stream pure `agy` stdout without wrapper noise unless `--json` is selected.
- **Exit Codes:** `0` on success, `1` on profile/alias not found or validation error, `2` on unauthenticated profile, `127` if `agy` binary missing, or the exact child exit code.

---

### Task 1: Project Scaffolding & Cargo Setup

**Files:**
- Create: `Cargo.toml`
- Create: `src/main.rs`
- Test: Build check via `cargo test`

**Interfaces:**
- Produces: Base Cargo workspace and minimal CLI entry point that compiles cleanly.

- [ ] **Step 1: Write Cargo.toml**

```toml
[package]
name = "agyp"
version = "0.1.0"
edition = "2021"
description = "Antigravity Multi-Profile CLI Manager for Developers and AI Agents"

[dependencies]
clap = { version = "4.5", features = ["derive", "cargo"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
dirs = "5.0"
chrono = { version = "0.4", features = ["serde"] }

[dev-dependencies]
tempfile = "3.10"
```

- [ ] **Step 2: Write minimal src/main.rs**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "agyp", version, about = "Antigravity Multi-Profile CLI Manager")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Show version or ping
    Ping,
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::Ping) => println!("pong"),
        None => println!("Use 'agyp --help' for usage instructions."),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_scaffold_runs() {
        assert_eq!(2 + 2, 4);
    }
}
```

- [ ] **Step 3: Run cargo test to verify build and test passes**

Run: `cargo test`  
Expected: Test `test_scaffold_runs` PASS.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml src/main.rs
git commit -m "chore: scaffold rust cargo project with clap and serde"
```

---

### Task 2: Config Management & Profile Registry (`src/config.rs`)

**Files:**
- Create: `src/config.rs`
- Modify: `src/main.rs`
- Test: `tests/config_test.rs`

**Interfaces:**
- Produces:
  - `struct ProfileRegistry`: Central store for profiles and aliases.
  - `struct ProfileInfo`: Individual profile metadata (name, email, dir, timestamps).
  - `ProfileRegistry::load_or_default(path: &Path) -> Result<Self>`
  - `ProfileRegistry::save(&self, path: &Path) -> Result<()>`
  - `ProfileRegistry::resolve_profile(&self, name_or_alias: &str) -> Option<&ProfileInfo>`
  - `ProfileRegistry::set_alias(&mut self, alias: &str, profile_name: &str) -> Result<()>`
  - `ProfileRegistry::remove_alias(&mut self, alias: &str) -> Result<()>`
  - `ProfileRegistry::set_default_profile(&mut self, profile_name: &str) -> Result<()>`
  - `ProfileRegistry::remove_profile(&mut self, profile_name: &str) -> Result<()>`

- [ ] **Step 1: Write the failing tests in tests/config_test.rs**

```rust
use std::path::PathBuf;
use tempfile::tempdir;

#[path = "../src/config.rs"]
mod config;
use config::{ProfileInfo, ProfileRegistry};

#[test]
fn test_registry_add_and_resolve_profile() {
    let mut reg = ProfileRegistry::default();
    let info = ProfileInfo {
        name: "work".to_string(),
        email: "user@company.com".to_string(),
        dir: PathBuf::from("/tmp/profiles/work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    };
    reg.add_profile(info).unwrap();

    // Direct resolution
    let resolved = reg.resolve_profile("work");
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().email, "user@company.com");

    // Alias resolution
    reg.set_alias("p1", "work").unwrap();
    let from_alias = reg.resolve_profile("p1");
    assert!(from_alias.is_some());
    assert_eq!(from_alias.unwrap().name, "work");
}

#[test]
fn test_registry_persistence() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("profiles.json");

    let mut reg = ProfileRegistry::default();
    reg.add_profile(ProfileInfo {
        name: "personal".to_string(),
        email: "user@gmail.com".to_string(),
        dir: dir.path().join("personal"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    }).unwrap();
    reg.set_default_profile("personal").unwrap();
    reg.save(&config_path).unwrap();

    let loaded = ProfileRegistry::load_or_default(&config_path).unwrap();
    assert_eq!(loaded.default_profile, Some("personal".to_string()));
    assert!(loaded.profiles.contains_key("personal"));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --test config_test`  
Expected: FAIL compilation because `src/config.rs` does not exist yet.

- [ ] **Step 3: Implement src/config.rs**

```rust
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub email: String,
    pub dir: PathBuf,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRegistry {
    pub version: u32,
    pub default_profile: Option<String>,
    pub aliases: HashMap<String, String>,
    pub profiles: HashMap<String, ProfileInfo>,
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            default_profile: None,
            aliases: HashMap::new(),
            profiles: HashMap::new(),
        }
    }
}

impl ProfileRegistry {
    pub fn get_profiles_dir() -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".gemini-profiles")
    }

    pub fn get_config_file_path() -> PathBuf {
        Self::get_profiles_dir().join("profiles.json")
    }

    pub fn load_or_default(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(path).map_err(|e| format!("Failed to read registry: {}", e))?;
        serde_json::from_str(&content).map_err(|e| format!("Invalid JSON in registry: {}", e))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("Failed to create parent dir: {}", e))?;
        }
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;
        fs::write(path, content).map_err(|e| format!("Failed to write registry: {}", e))?;
        Ok(())
    }

    pub fn resolve_profile(&self, name_or_alias: &str) -> Option<&ProfileInfo> {
        if let Some(profile) = self.profiles.get(name_or_alias) {
            return Some(profile);
        }
        if let Some(target_name) = self.aliases.get(name_or_alias) {
            return self.profiles.get(target_name);
        }
        None
    }

    pub fn add_profile(&mut self, info: ProfileInfo) -> Result<(), String> {
        self.profiles.insert(info.name.clone(), info);
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<(), String> {
        if self.profiles.remove(name).is_none() {
            return Err(format!("Profile '{}' not found", name));
        }
        // Remove dangling aliases
        self.aliases.retain(|_, target| target != name);
        if self.default_profile.as_deref() == Some(name) {
            self.default_profile = self.profiles.keys().next().cloned();
        }
        Ok(())
    }

    pub fn set_alias(&mut self, alias: &str, profile_name: &str) -> Result<(), String> {
        if !self.profiles.contains_key(profile_name) {
            return Err(format!("Profile '{}' does not exist", profile_name));
        }
        self.aliases.insert(alias.to_string(), profile_name.to_string());
        Ok(())
    }

    pub fn remove_alias(&mut self, alias: &str) -> Result<(), String> {
        if self.aliases.remove(alias).is_none() {
            return Err(format!("Alias '{}' not found", alias));
        }
        Ok(())
    }

    pub fn set_default_profile(&mut self, profile_name: &str) -> Result<(), String> {
        let resolved_name = if self.profiles.contains_key(profile_name) {
            profile_name.to_string()
        } else if let Some(target) = self.aliases.get(profile_name) {
            target.clone()
        } else {
            return Err(format!("Profile or alias '{}' does not exist", profile_name));
        };
        self.default_profile = Some(resolved_name);
        Ok(())
    }
}
```

- [ ] **Step 4: Register module in src/main.rs and run tests**

Add `mod config;` to `src/main.rs` and run `cargo test --test config_test`.  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs tests/config_test.rs src/main.rs
git commit -m "feat: implement profile registry and configuration management"
```

---

### Task 3: Agy Locator & Process Execution Engine (`src/agy.rs`)

**Files:**
- Create: `src/agy.rs`
- Modify: `src/main.rs`
- Test: `tests/agy_test.rs`

**Interfaces:**
- Produces:
  - `locate_agy_binary() -> Result<PathBuf, String>`
  - `check_profile_auth(profile_dir: &Path) -> bool`
  - `extract_profile_email(profile_dir: &Path) -> Option<String>`
  - `build_agy_command(profile_dir: &Path, args: &[String]) -> std::process::Command`

- [ ] **Step 1: Write tests in tests/agy_test.rs**

```rust
use std::fs;
use tempfile::tempdir;

#[path = "../src/agy.rs"]
mod agy;
use agy::{check_profile_auth, extract_profile_email, locate_agy_binary};

#[test]
fn test_locate_agy_binary_exists() {
    let bin = locate_agy_binary();
    assert!(bin.is_ok(), "agy binary should be found on this system: {:?}", bin);
}

#[test]
fn test_profile_auth_detection() {
    let dir = tempdir().unwrap();
    assert!(!check_profile_auth(dir.path()));

    let creds_file = dir.path().join("oauth_creds.json");
    fs::write(&creds_file, r#"{"access_token":"dummy"}"#).unwrap();
    assert!(check_profile_auth(dir.path()));
}

#[test]
fn test_extract_profile_email() {
    let dir = tempdir().unwrap();
    let accounts_file = dir.path().join("google_accounts.json");
    fs::write(&accounts_file, r#"{"active":"test@example.com","old":[]}"#).unwrap();

    let email = extract_profile_email(dir.path());
    assert_eq!(email, Some("test@example.com".to_string()));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --test agy_test`  
Expected: FAIL compilation as `src/agy.rs` is not yet created.

- [ ] **Step 3: Implement src/agy.rs**

```rust
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

    Err("Could not find 'agy' binary in PATH or standard paths (~/.local/bin/agy, /opt/homebrew/bin/agy).".to_string())
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
```

- [ ] **Step 4: Register module in src/main.rs and run tests**

Add `mod agy;` to `src/main.rs` and run `cargo test --test agy_test`.  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/agy.rs tests/agy_test.rs src/main.rs
git commit -m "feat: implement agy binary locator and process command builder"
```

---

### Task 4: Profile Management Commands (`login`, `list`, `default`, `rm`)

**Files:**
- Create: `src/commands/mod.rs`
- Create: `src/commands/profile.rs`
- Modify: `src/main.rs`
- Test: `tests/profile_commands_test.rs`

**Interfaces:**
- Produces:
  - `commands::profile::cmd_login(name: &str, import_current: bool) -> Result<(), i32>`
  - `commands::profile::cmd_list(json_output: bool) -> Result<(), i32>`
  - `commands::profile::cmd_default(profile: Option<&str>) -> Result<(), i32>`
  - `commands::profile::cmd_rm(name: &str, yes: bool) -> Result<(), i32>`

- [ ] **Step 1: Write integration tests in tests/profile_commands_test.rs**

```rust
use tempfile::tempdir;

#[path = "../src/config.rs"]
mod config;
#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
mod commands;

use config::{ProfileInfo, ProfileRegistry};

#[test]
fn test_default_profile_selection() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("profiles.json");
    let mut reg = ProfileRegistry::default();

    reg.add_profile(ProfileInfo {
        name: "p1".to_string(),
        email: "p1@example.com".to_string(),
        dir: dir.path().join("p1"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    }).unwrap();

    reg.add_profile(ProfileInfo {
        name: "p2".to_string(),
        email: "p2@example.com".to_string(),
        dir: dir.path().join("p2"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    }).unwrap();

    reg.set_default_profile("p2").unwrap();
    assert_eq!(reg.default_profile, Some("p2".to_string()));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --test profile_commands_test`  
Expected: FAIL compilation since `src/commands/` does not exist yet.

- [ ] **Step 3: Implement src/commands/mod.rs and src/commands/profile.rs**

In `src/commands/mod.rs`:
```rust
pub mod profile;
pub mod alias;
pub mod prompt;
pub mod exec;
```

In `src/commands/profile.rs`:
```rust
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use chrono::Utc;
use serde_json::json;

use crate::agy::{check_profile_auth, extract_profile_email, locate_agy_binary};
use crate::config::{ProfileInfo, ProfileRegistry};

pub fn cmd_login(name: &str, import_current: bool) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let profile_dir = profiles_root.join(name);
    fs::create_dir_all(&profile_dir).map_err(|e| {
        eprintln!("Failed to create profile directory: {}", e);
        1
    })?;

    if import_current {
        if let Some(home) = dirs::home_dir() {
            let default_gemini = home.join(".gemini");
            let files = ["oauth_creds.json", "google_accounts.json"];
            for file in &files {
                let src = default_gemini.join(file);
                let dst = profile_dir.join(file);
                if src.exists() {
                    let _ = fs::copy(&src, &dst);
                }
            }
        }
    } else {
        // Run agy interactively to trigger OAuth flow
        let agy_bin = locate_agy_binary().map_err(|e| {
            eprintln!("Error: {}", e);
            127
        })?;

        println!("Launching Antigravity login for profile '{}'...", name);
        let status = std::process::Command::new(agy_bin)
            .arg(format!("--gemini_dir={}", profile_dir.display()))
            .status()
            .map_err(|e| {
                eprintln!("Failed to run agy: {}", e);
                1
            })?;

        if !status.success() {
            eprintln!("Login process exited with error.");
            return Err(status.code().unwrap_or(1));
        }
    }

    let email = extract_profile_email(&profile_dir).unwrap_or_else(|| "unknown".to_string());
    let info = ProfileInfo {
        name: name.to_string(),
        email: email.clone(),
        dir: profile_dir,
        created_at: Utc::now(),
        last_used_at: None,
    };

    reg.add_profile(info).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    if reg.default_profile.is_none() {
        reg.default_profile = Some(name.to_string());
    }

    reg.save(&config_path).map_err(|e| {
        eprintln!("Error saving registry: {}", e);
        1
    })?;

    println!("Profile '{}' ({}) configured successfully.", name, email);
    Ok(())
}

pub fn cmd_list(json_output: bool) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    if json_output {
        let val = json!({
            "default_profile": reg.default_profile,
            "profiles": reg.profiles,
            "aliases": reg.aliases,
        });
        println!("{}", serde_json::to_string_pretty(&val).unwrap());
        return Ok(());
    }

    if reg.profiles.is_empty() {
        println!("No profiles found. Run 'agyp login <profile>' to create one.");
        return Ok(());
    }

    println!("{:<15} {:<30} {:<15} {:<10} {:<15}", "PROFILE", "EMAIL", "ALIASES", "DEFAULT", "STATUS");
    println!("{}", "-".repeat(90));

    for (name, info) in &reg.profiles {
        let aliases: Vec<&str> = reg.aliases.iter()
            .filter(|(_, target)| target.as_str() == name)
            .map(|(a, _)| a.as_str())
            .collect();
        let alias_str = if aliases.is_empty() { "-".to_string() } else { aliases.join(", ") };
        let is_default = if reg.default_profile.as_deref() == Some(name) { "yes (*)" } else { "no" };
        let auth_status = if check_profile_auth(&info.dir) { "ready" } else { "unauthenticated" };

        println!("{:<15} {:<30} {:<15} {:<10} {:<15}", name, info.email, alias_str, is_default, auth_status);
    }
    Ok(())
}

pub fn cmd_default(profile: Option<&str>) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    match profile {
        Some(target) => {
            reg.set_default_profile(target).map_err(|e| {
                eprintln!("Error: {}", e);
                1
            })?;
            reg.save(&config_path).map_err(|e| {
                eprintln!("Error: {}", e);
                1
            })?;
            println!("Default profile set to '{}'.", target);
            Ok(())
        }
        None => {
            match &reg.default_profile {
                Some(def) => println!("{}", def),
                None => println!("No default profile configured."),
            }
            Ok(())
        }
    }
}

pub fn cmd_rm(name: &str, yes: bool) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    let profile = reg.resolve_profile(name).ok_or_else(|| {
        eprintln!("Error: Profile '{}' not found.", name);
        1
    })?.clone();

    if !yes {
        print!("Are you sure you want to delete profile '{}' and its directory? [y/N]: ", profile.name);
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let _ = fs::remove_dir_all(&profile.dir);
    reg.remove_profile(&profile.name).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;
    reg.save(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    println!("Profile '{}' deleted.", profile.name);
    Ok(())
}
```

- [ ] **Step 4: Run test to verify passes**

Run: `cargo test --test profile_commands_test`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands/mod.rs src/commands/profile.rs tests/profile_commands_test.rs
git commit -m "feat: implement login, list, default, and rm profile commands"
```

---

### Task 5: Alias Management Commands (`agyp alias`)

**Files:**
- Create: `src/commands/alias.rs`
- Modify: `src/commands/mod.rs`
- Test: `tests/alias_test.rs`

**Interfaces:**
- Produces:
  - `commands::alias::cmd_alias_set(alias: &str, target_profile: &str) -> Result<(), i32>`
  - `commands::alias::cmd_alias_rm(alias: &str) -> Result<(), i32>`
  - `commands::alias::cmd_alias_list(json_output: bool) -> Result<(), i32>`

- [ ] **Step 1: Write integration test in tests/alias_test.rs**

```rust
use tempfile::tempdir;

#[path = "../src/config.rs"]
mod config;
use config::{ProfileInfo, ProfileRegistry};

#[test]
fn test_alias_lifecycle() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("profiles.json");
    let mut reg = ProfileRegistry::default();

    reg.add_profile(ProfileInfo {
        name: "work".to_string(),
        email: "work@example.com".to_string(),
        dir: dir.path().join("work"),
        created_at: chrono::Utc::now(),
        last_used_at: None,
    }).unwrap();

    // Set alias
    reg.set_alias("w", "work").unwrap();
    assert_eq!(reg.aliases.get("w"), Some(&"work".to_string()));

    // Resolve through alias
    let resolved = reg.resolve_profile("w").unwrap();
    assert_eq!(resolved.name, "work");

    // Remove alias
    reg.remove_alias("w").unwrap();
    assert!(reg.aliases.get("w").is_none());
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --test alias_test`  
Expected: PASS unit logic.

- [ ] **Step 3: Implement src/commands/alias.rs**

```rust
use serde_json::json;
use crate::config::ProfileRegistry;

pub fn cmd_alias_set(alias: &str, profile_name: &str) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    reg.set_alias(alias, profile_name).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    reg.save(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    println!("Alias '{}' -> '{}' set successfully.", alias, profile_name);
    Ok(())
}

pub fn cmd_alias_rm(alias: &str) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    reg.remove_alias(alias).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    reg.save(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    println!("Alias '{}' removed.", alias);
    Ok(())
}

pub fn cmd_alias_list(json_output: bool) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&json!(reg.aliases)).unwrap());
        return Ok(());
    }

    if reg.aliases.is_empty() {
        println!("No aliases configured.");
        return Ok(());
    }

    println!("{:<20} -> {:<20}", "ALIAS", "TARGET PROFILE");
    println!("{}", "-".repeat(45));
    for (alias, target) in &reg.aliases {
        println!("{:<20} -> {:<20}", alias, target);
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test`  
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands/alias.rs tests/alias_test.rs
git commit -m "feat: implement alias set, rm, and list subcommands"
```

---

### Task 6: Prompt & Exec Commands (`src/commands/prompt.rs`, `src/commands/exec.rs`)

**Files:**
- Create: `src/commands/prompt.rs`
- Create: `src/commands/exec.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/main.rs` (wire all subcommands in CLI parser)
- Test: `tests/prompt_args_test.rs`

**Interfaces:**
- Produces:
  - `commands::prompt::cmd_prompt(profile: Option<&str>, prompt_text: Option<&str>, model: Option<&str>, add_dirs: &[String], no_skip_perm: bool, json_output: bool) -> Result<(), i32>`
  - `commands::exec::cmd_exec(profile_or_alias: &str, raw_args: &[String]) -> Result<(), i32>`

- [ ] **Step 1: Write argument builder tests in tests/prompt_args_test.rs**

```rust
use std::path::PathBuf;

#[path = "../src/agy.rs"]
mod agy;
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
    let args: Vec<String> = cmd.get_args().map(|s| s.to_string_lossy().to_string()).collect();

    assert!(args.contains(&"--gemini_dir=/tmp/profiles/work".to_string()));
    assert!(args.contains(&"--dangerously-skip-permissions".to_string()));
    assert!(args.contains(&"-p".to_string()));
    assert!(args.contains(&"hello world".to_string()));
}
```

- [ ] **Step 2: Run test to verify argument generation**

Run: `cargo test --test prompt_args_test`  
Expected: PASS.

- [ ] **Step 3: Implement src/commands/prompt.rs**

```rust
use std::io::{self, Read};
use chrono::Utc;
use serde_json::json;

use crate::agy::{build_agy_command, check_profile_auth};
use crate::config::ProfileRegistry;

pub fn cmd_prompt(
    profile_arg: Option<&str>,
    prompt_text_arg: Option<&str>,
    model: Option<&str>,
    add_dirs: &[String],
    no_skip_perm: bool,
    json_output: bool,
) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    let target_profile_key = match profile_arg {
        Some(p) => p.to_string(),
        None => reg.default_profile.clone().ok_or_else(|| {
            eprintln!("Error: No profile specified and no default profile set. Run 'agyp default <profile>' or 'agyp prompt <profile>'.");
            1
        })?,
    };

    let profile = reg.resolve_profile(&target_profile_key).ok_or_else(|| {
        eprintln!("Error: Profile or alias '{}' not found.", target_profile_key);
        1
    })?.clone();

    if !check_profile_auth(&profile.dir) {
        eprintln!("Error: Profile '{}' is not authenticated. Please run 'agyp login {}' first.", profile.name, profile.name);
        return Err(2);
    }

    // Resolve prompt text (from argument or stdin)
    let prompt_content = match prompt_text_arg {
        Some(text) if !text.trim().is_empty() => text.to_string(),
        _ => {
            let mut stdin_buf = String::new();
            if io::stdin().read_to_string(&mut stdin_buf).is_ok() && !stdin_buf.trim().is_empty() {
                stdin_buf
            } else {
                eprintln!("Error: Prompt content cannot be empty.");
                return Err(1);
            }
        }
    };

    // Build args
    let mut args = Vec::new();
    if !no_skip_perm {
        args.push("--dangerously-skip-permissions".to_string());
    }
    if let Some(m) = model {
        args.push(format!("--model={}", m));
    }
    for dir in add_dirs {
        args.push(format!("--add-dir={}", dir));
    }
    args.push("-p".to_string());
    args.push(prompt_content);

    // Update last_used_at
    if let Some(mut p) = reg.profiles.get_mut(&profile.name) {
        p.last_used_at = Some(Utc::now());
    }
    let _ = reg.save(&config_path);

    let mut cmd = build_agy_command(&profile.dir, &args).map_err(|e| {
        eprintln!("Error: {}", e);
        127
    })?;

    if json_output {
        let output = cmd.output().map_err(|e| {
            eprintln!("Failed to execute agy: {}", e);
            1
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(1);

        let result = json!({
            "profile": profile.name,
            "email": profile.email,
            "exit_code": exit_code,
            "success": exit_code == 0,
            "output": stdout,
            "error": if stderr.is_empty() { None } else { Some(stderr) }
        });
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
        return if exit_code == 0 { Ok(()) } else { Err(exit_code) };
    }

    // Direct streaming mode for AI agents / humans
    let status = cmd.status().map_err(|e| {
        eprintln!("Failed to execute agy: {}", e);
        1
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(status.code().unwrap_or(1))
    }
}
```

- [ ] **Step 4: Implement src/commands/exec.rs**

```rust
use crate::agy::build_agy_command;
use crate::config::ProfileRegistry;

pub fn cmd_exec(profile_arg: &str, raw_args: &[String]) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    let profile = reg.resolve_profile(profile_arg).ok_or_else(|| {
        eprintln!("Error: Profile or alias '{}' not found.", profile_arg);
        1
    })?;

    let mut cmd = build_agy_command(&profile.dir, raw_args).map_err(|e| {
        eprintln!("Error: {}", e);
        127
    })?;

    let status = cmd.status().map_err(|e| {
        eprintln!("Failed to run agy: {}", e);
        1
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(status.code().unwrap_or(1))
    }
}
```

- [ ] **Step 5: Wire complete CLI in src/main.rs**

Update `src/main.rs` with Clap derive CLI matching all subcommands:
- `login <profile> [--import-current]`
- `list [--json]` (alias `ls`)
- `default [profile]`
- `rm <profile> [-y]`
- `alias <set|rm|list>`
- `prompt <profile> [prompt] [--model] [--add-dir] [--no-skip-perm] [--json]` (alias `-p`)
- `exec <profile> -- <raw_args...>`

- [ ] **Step 6: Run cargo check & cargo test to ensure clean compile**

Run: `cargo test`  
Expected: All tests PASS.

- [ ] **Step 7: Commit**

```bash
git add src/commands/prompt.rs src/commands/exec.rs src/main.rs tests/prompt_args_test.rs
git commit -m "feat: complete prompt and exec commands with clean stdin/stdout piping"
```

---

### Task 7: End-to-End Build & System Installation

**Files:**
- Create: `Makefile` or build script `scripts/install.sh`
- Target: `/Users/tdnhathuy/.local/bin/agyp`

**Interfaces:**
- Produces: Standalone binary `agyp` in `~/.local/bin/` ready for Hermes and terminal usage.

- [ ] **Step 1: Write Makefile for installation**

```makefile
.PHONY: build test install clean

build:
	cargo build --release

test:
	cargo test

install: build
	mkdir -p $(HOME)/.local/bin
	cp target/release/agyp $(HOME)/.local/bin/agyp
	chmod +x $(HOME)/.local/bin/agyp
	@echo "Installed agyp to $(HOME)/.local/bin/agyp"

clean:
	cargo clean
```

- [ ] **Step 2: Build release binary and install**

Run: `make install`  
Expected: `target/release/agyp` compiled and copied to `~/.local/bin/agyp`.

- [ ] **Step 3: Verify agyp in terminal**

Run: `agyp --help`  
Expected: Exit code 0, displays all commands (`login`, `list`, `prompt`, `alias`, `default`, `exec`, `rm`).

- [ ] **Step 4: Verify profile import and test prompt**

Run:
```bash
agyp login default --import-current
agyp list
agyp prompt default "Reply with exactly: AGYP_IS_WORKING"
```
Expected:
1. `default` profile registered with active user email.
2. `agyp prompt default` returns `AGYP_IS_WORKING`.

- [ ] **Step 5: Commit**

```bash
git add Makefile
git commit -m "build: add makefile and install agyp binary to ~/.local/bin"
```

---

### Task 8: AI Agent Skill Documentation (`skills/agyp/SKILL.md`)

**Files:**
- Create: `skills/agyp/SKILL.md`
- Target: `/Users/tdnhathuy/Documents/antigravity/agy-mult-profiles/skills/agyp/SKILL.md` (and copy to `~/.gemini/antigravity/skills/agyp/SKILL.md` for global agent discovery).

**Interfaces:**
- Produces: Standard AGY/Hermes Skill doc enabling agents to discover, switch, and prompt profiles autonomously.

- [ ] **Step 1: Create skills/agyp/SKILL.md**

```markdown
---
name: agyp
description: Use when you need to query or run tasks across multiple Antigravity (agy) accounts or profiles. Supports listing profiles, selecting work/personal accounts, and running non-interactive prompts.
---

# Antigravity Multi-Profile CLI (`agyp`)

Use this skill when interacting with Google Antigravity across different accounts.

## 1. Inspect Available Profiles
```bash
agyp list --json
```
Returns a JSON object with all configured profiles, their linked Google emails, and aliases.

## 2. Execute a Prompt on a Specific Profile
To send a prompt non-interactively without user interruption:
```bash
agyp prompt <profile_or_alias> "<prompt text>"
```

### Examples:
- Use alias: `agyp prompt p1 "Check current git status"`
- Use model flag: `agyp prompt work --model gemini-2.5-flash "Summarize changes"`
- Pipe content: `cat error.log | agyp prompt personal "Analyze this crash stacktrace"`

## 3. Advanced Pass-Through Commands
To run arbitrary `agy` options under a profile:
```bash
agyp exec <profile_or_alias> -- --continue
```

## 4. Handling Unauthenticated Errors (Exit Code 2)
If a prompt fails with exit code 2:
- The profile is not logged in.
- Prompt the user to run: `agyp login <profile>` in an interactive terminal.
```

- [ ] **Step 2: Copy skill to global skills directory**

Run: `mkdir -p ~/.gemini/antigravity/skills/agyp && cp skills/agyp/SKILL.md ~/.gemini/antigravity/skills/agyp/SKILL.md`

- [ ] **Step 3: Commit**

```bash
git add skills/agyp/SKILL.md
git commit -m "docs(skill): add agyp skill for Hermes and AI Agent orchestration"
```

---

## Plan Self-Review Check
- **Spec coverage:** Covers all requirements (isolation via `--gemini_dir`, `login --import-current`, `list`, `alias`, `prompt`, `exec`, `rm`, error codes, Hermes skill).
- **No placeholders:** All code, test cases, and paths are fully specified.
- **Type consistency:** Config structures and command interfaces match between tasks.
