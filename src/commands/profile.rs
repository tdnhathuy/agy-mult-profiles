#![allow(dead_code)]

use chrono::Utc;
use serde_json::json;
use std::fs;
use std::io::{self, Write};

use crate::agy::{check_profile_auth, extract_profile_email, locate_agy_binary};
use crate::config::{ProfileInfo, ProfileRegistry};

pub fn cmd_login(name: &str, import_current: bool) -> Result<(), i32> {
    crate::security::validate_component(name, "profile name").map_err(|e| {
        eprintln!("Error: {e}");
        1
    })?;
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;
    let lock_path = ProfileRegistry::get_profiles_dir().join("agyp.lock");
    let _process_lock = crate::security::ProcessLock::acquire(&lock_path).map_err(|e| {
        eprintln!("Failed to serialize AGY profile access: {e}");
        1
    })?;

    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    crate::security::secure_dir(&profiles_root).map_err(|e| {
        eprintln!("Failed to secure profiles directory: {e}");
        1
    })?;
    let profile_dir = profiles_root.join(name);
    crate::security::secure_dir(&profile_dir).map_err(|e| {
        eprintln!("Failed to create profile directory: {}", e);
        1
    })?;

    if import_current {
        let creds_file = dirs::home_dir().map(|h| h.join(".gemini/oauth_creds.json"));
        let creds_exist = creds_file.as_ref().map(|p| p.is_file()).unwrap_or(false);

        if !creds_exist {
            eprintln!("Warning: No active credentials found in ~/.gemini/oauth_creds.json. Please run 'agyp login <profile>' without --import-current to authenticate.");
        }

        // 1. Try importing token from system Keychain
        if let Ok(Some(token)) = crate::keychain::read_system_keychain_token() {
            let _ = crate::keychain::save_token_to_profile(&profile_dir, &token);
        }

        // 2. Also copy ~/.gemini files if available
        if let Some(home) = dirs::home_dir() {
            let default_gemini = home.join(".gemini");
            let files = [
                "oauth_creds.json",
                "google_accounts.json",
                "jetski-standalone-oauth-token",
            ];
            for file in &files {
                let src = default_gemini.join(file);
                let dst = profile_dir.join(file);
                if src.exists() {
                    crate::security::reject_symlink(&dst).map_err(|e| {
                        eprintln!("Refusing unsafe credential destination: {e}");
                        1
                    })?;
                    fs::copy(&src, &dst).map_err(|e| {
                        eprintln!("Failed to import credential file '{}': {e}", file);
                        1
                    })?;
                    crate::security::set_private_file_permissions(&dst).map_err(|e| {
                        eprintln!("Failed to secure imported credential file '{}': {e}", file);
                        1
                    })?;
                }
            }
        }
    } else {
        // Fresh interactive login for a new account:
        // 1. Back up the exact Keychain state and restore it after login.
        let prev_keychain_token = match crate::keychain::read_system_keychain_token() {
            Ok(tok) => tok,
            Err(e) => {
                eprintln!("Error checking Keychain before login: {e}");
                return Err(1);
            }
        };

        // 2. Remove the existing token so AGY cannot silently reuse the old account.
        let _ = crate::keychain::delete_system_keychain_token();

        let agy_bin = match locate_agy_binary() {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Error: {}", e);
                let restore = match prev_keychain_token {
                    Some(ref prev) => crate::keychain::write_system_keychain_token(prev),
                    None => crate::keychain::delete_system_keychain_token(),
                };
                if let Err(restore_err) = restore {
                    eprintln!("Failed to restore prior Keychain state: {restore_err}");
                }
                return Err(127);
            }
        };

        println!("Initiating Google OAuth login for profile '{}'...", name);
        println!("Please sign in with the Google account for this profile in your browser.");

        let mut login_cmd = std::process::Command::new(agy_bin);
        login_cmd.arg(format!("--gemini_dir={}", profile_dir.display()));
        let status_res = crate::agy::run_command_interruptibly(&mut login_cmd);

        let persist_result = match crate::keychain::read_system_keychain_token() {
            Ok(Some(token)) => crate::keychain::save_token_to_profile(&profile_dir, &token)
                .map(|_| ())
                .map_err(|e| format!("Failed to save profile credential: {e}")),
            Ok(None) => {
                Err("Login was cancelled or failed to authenticate with Google.".to_string())
            }
            Err(e) => Err(format!("Failed to read Keychain token after login: {e}")),
        };
        let restore_result = match prev_keychain_token {
            Some(prev) => crate::keychain::write_system_keychain_token(&prev),
            None => crate::keychain::delete_system_keychain_token(),
        };

        if let Err(e) = restore_result {
            eprintln!("Failed to restore prior Keychain state: {e}");
            return Err(1);
        }
        if let Err(e) = persist_result {
            eprintln!("{e}");
            return Err(status_res.map(|s| s.code().unwrap_or(1)).unwrap_or(1));
        }
        match status_res {
            Ok(status) if status.success() => {
                println!("Authentication completed successfully.");
            }
            Ok(status) => {
                eprintln!("AGY login exited unsuccessfully after writing credentials.");
                return Err(status.code().unwrap_or(1));
            }
            Err(e) => {
                eprintln!("{e}");
                return Err(1);
            }
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

    println!(
        "{:<15} {:<30} {:<15} {:<10} {:<15}",
        "PROFILE", "EMAIL", "ALIASES", "DEFAULT", "STATUS"
    );
    println!("{}", "-".repeat(90));

    for (name, info) in &reg.profiles {
        let aliases: Vec<&str> = reg
            .aliases
            .iter()
            .filter(|(_, target)| target.as_str() == name)
            .map(|(a, _)| a.as_str())
            .collect();
        let alias_str = if aliases.is_empty() {
            "-".to_string()
        } else {
            aliases.join(", ")
        };
        let is_default = if reg.default_profile.as_deref() == Some(name) {
            "yes (*)"
        } else {
            "no"
        };
        let auth_status = if check_profile_auth(&info.dir) {
            "ready"
        } else {
            "unauthenticated"
        };

        println!(
            "{:<15} {:<30} {:<15} {:<10} {:<15}",
            name, info.email, alias_str, is_default, auth_status
        );
    }
    Ok(())
}

pub fn cmd_default(profile_arg: Option<&str>) -> Result<(), i32> {
    if let Some(name) = profile_arg {
        crate::security::validate_component(name, "profile name or alias").map_err(|e| {
            eprintln!("Error: {e}");
            1
        })?;
    }
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    match profile_arg {
        Some(target) => {
            let prof_dir = reg.resolve_profile(target).map(|p| p.dir.clone());
            reg.set_default_profile(target).map_err(|e| {
                eprintln!("Error: {}", e);
                1
            })?;
            reg.save(&config_path).map_err(|e| {
                eprintln!("Error saving registry: {}", e);
                1
            })?;

            if let Some(dir) = prof_dir {
                let _ = crate::keychain::activate_profile_token(&dir);
            }

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
    crate::security::validate_component(name, "profile name or alias").map_err(|e| {
        eprintln!("Error: {e}");
        1
    })?;
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    let profile = reg
        .resolve_profile(name)
        .ok_or_else(|| {
            eprintln!("Error: Profile '{}' not found.", name);
            1
        })?
        .clone();

    if !yes {
        print!(
            "Are you sure you want to delete profile '{}' and its directory? [y/N]: ",
            profile.name
        );
        io::stdout().flush().unwrap();
        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let safe_profile_dir = crate::security::validate_profile_dir(&profile.dir).map_err(|e| {
        eprintln!("Refusing to delete unsafe profile path: {e}");
        1
    })?;
    fs::remove_dir_all(&safe_profile_dir).map_err(|e| {
        eprintln!("Failed to delete profile directory: {e}");
        1
    })?;
    reg.remove_profile(&profile.name).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;
    reg.save(&config_path).map_err(|e| {
        eprintln!("Error saving registry: {}", e);
        1
    })?;

    println!("Profile '{}' deleted.", profile.name);
    Ok(())
}
