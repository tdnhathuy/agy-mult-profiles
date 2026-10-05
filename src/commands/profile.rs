#![allow(dead_code)]

use chrono::Utc;
use serde_json::json;
use std::fs;
use std::io::{self, Write};
use std::time::Instant;

use crate::agy::{check_profile_auth, extract_profile_email, locate_agy_binary};
use crate::config::{ProfileInfo, ProfileRegistry};
use crate::keychain::KeychainBackend;

pub fn cmd_login(name: &str, import_current: bool) -> Result<(), i32> {
    crate::security::validate_component(name, "profile name").map_err(|e| {
        eprintln!("Error: {e}");
        1
    })?;
    let config_path = ProfileRegistry::get_config_file_path();
    let lock_path = crate::security::profile_lock_path(&ProfileRegistry::get_profiles_dir(), name);
    let process_lock = crate::security::ProcessLock::acquire_announcing(&lock_path, || {
        eprintln!("Profile '{name}' is in use by another agyp command; waiting...")
    })
    .map_err(|e| {
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
            // The isolated Keychain wins over keychain_token.json once populated; drop any
            // token from a previous account so the next run re-seeds from the imported one.
            crate::keychain::invalidate_isolated_token(
                &profile_dir,
                Instant::now() + crate::keychain::PROFILE_KEYCHAIN_SETUP_TIMEOUT,
            )
            .map_err(|e| {
                eprintln!("Failed to reset isolated profile Keychain: {e}");
                1
            })?;
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
        // Fresh interactive login runs AGY inside the profile's isolated HOME, so OAuth writes
        // straight into the profile Keychain and the shared system Keychain is never touched.
        let agy_bin = locate_agy_binary().map_err(|e| {
            eprintln!("Error: {}", e);
            127
        })?;
        let isolated = crate::keychain::prepare_isolated_home(
            &profile_dir,
            Instant::now() + crate::keychain::PROFILE_KEYCHAIN_SETUP_TIMEOUT,
        )
        .map_err(|e| {
            eprintln!("Failed to prepare isolated profile Keychain: {e}");
            1
        })?;
        let profile_keychain = isolated.backend();
        // Remove any leftover token so AGY cannot silently reuse a previous account.
        profile_keychain.delete_token().map_err(|e| {
            eprintln!("Failed to clear profile Keychain before login: {e}");
            1
        })?;

        println!("Initiating Google OAuth login for profile '{}'...", name);
        println!("Please sign in with the Google account for this profile in your browser.");

        let mut login_cmd = std::process::Command::new(agy_bin);
        login_cmd.arg(format!("--gemini_dir={}", profile_dir.display()));
        crate::keychain::apply_profile_home(&mut login_cmd, Some(&isolated.home));
        let login = run_login_session(
            &mut login_cmd,
            &profile_keychain,
            &config_path,
            name,
            &profile_dir,
            process_lock,
        );

        // AGY may update the token after OAuth (onboarding, refresh); mirror its final state.
        match profile_keychain.read_token() {
            Ok(Some(token)) => {
                let _lock = crate::security::FileLock::acquire_with_timeout(
                    &lock_path,
                    crate::keychain::PROFILE_KEYCHAIN_SETUP_TIMEOUT,
                );
                if let Err(e) = crate::keychain::save_token_to_profile(&profile_dir, &token) {
                    eprintln!("Failed to save profile credential: {e}");
                    return Err(1);
                }
            }
            Ok(None) if login.registered => {
                eprintln!("Warning: AGY signed out after login; run 'agyp login {name}' again.");
            }
            Ok(None) => {
                eprintln!("Login was cancelled or failed to authenticate with Google.");
                return Err(login.status.map(|s| s.code().unwrap_or(1)).unwrap_or(1));
            }
            Err(e) => {
                eprintln!("Failed to read profile Keychain token after login: {e}");
                return Err(1);
            }
        }
        match login.status {
            Ok(_) => println!("Authentication completed successfully."),
            // A credential was stored, so an interrupted or failed AGY exit still logged in.
            Err(e) => eprintln!("{e}"),
        }
    }

    let email = register_profile(&config_path, name, &profile_dir).map_err(|e| {
        eprintln!("Error saving registry: {}", e);
        1
    })?;

    println!("Profile '{}' ({}) configured successfully.", name, email);
    Ok(())
}

/// Adds or refreshes the profile in the registry, keeping its original creation time.
fn register_profile(
    config_path: &std::path::Path,
    name: &str,
    profile_dir: &std::path::Path,
) -> Result<String, String> {
    let email = extract_profile_email(profile_dir).unwrap_or_else(|| "unknown".to_string());
    ProfileRegistry::update(config_path, |reg| {
        let created_at = reg
            .profiles
            .get(name)
            .map(|p| p.created_at)
            .unwrap_or_else(Utc::now);
        reg.add_profile(ProfileInfo {
            name: name.to_string(),
            email: email.clone(),
            dir: profile_dir.to_path_buf(),
            created_at,
            last_used_at: None,
        })?;
        if reg.default_profile.is_none() {
            reg.default_profile = Some(name.to_string());
        }
        Ok(())
    })?;
    Ok(email)
}

struct LoginSession {
    status: Result<std::process::ExitStatus, String>,
    registered: bool,
}

/// Runs the interactive AGY login. As soon as OAuth stores a token in the profile Keychain, the
/// profile is registered and its lock released, so it shows up in `agyp ls` and other agyp
/// commands can use it while this AGY session stays open.
fn run_login_session(
    cmd: &mut std::process::Command,
    profile_keychain: &crate::keychain::IsolatedKeychainBackend,
    config_path: &std::path::Path,
    name: &str,
    profile_dir: &std::path::Path,
    process_lock: crate::security::ProcessLock,
) -> LoginSession {
    use std::process::Stdio;
    use std::time::Duration;

    const TOKEN_POLL_INTERVAL: Duration = Duration::from_secs(1);

    let mut process_lock = Some(process_lock);
    // Keeps signal routing alive after the profile lock is released.
    let signals = match crate::security::SignalGuard::install() {
        Ok(signals) => signals,
        Err(e) => {
            return LoginSession {
                status: Err(e),
                registered: false,
            }
        }
    };
    let mut child = match cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return LoginSession {
                status: Err(format!("Failed to execute agy: {e}")),
                registered: false,
            }
        }
    };

    let mut registered = false;
    let mut last_poll: Option<Instant> = None;
    let status = loop {
        if crate::security::ProcessLock::interrupted() {
            let _ = child.kill();
            let _ = child.wait();
            break Err("Interrupted while AGY login was running".to_string());
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(e) => break Err(format!("Failed to wait for agy: {e}")),
        }
        if !registered && last_poll.is_none_or(|t| t.elapsed() >= TOKEN_POLL_INTERVAL) {
            last_poll = Some(Instant::now());
            if let Ok(Some(token)) = profile_keychain.read_token() {
                if crate::keychain::save_token_to_profile(profile_dir, &token).is_ok()
                    && register_profile(config_path, name, profile_dir).is_ok()
                {
                    registered = true;
                    process_lock.take();
                }
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    drop(signals);
    LoginSession { status, registered }
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
    match profile_arg {
        Some(target) => {
            let prof_dir = ProfileRegistry::update(&config_path, |reg| {
                let prof_dir = reg.resolve_profile(target).map(|p| p.dir.clone());
                reg.set_default_profile(target)?;
                Ok(prof_dir)
            })
            .map_err(|e| {
                eprintln!("Error: {}", e);
                1
            })?;

            if let Some(dir) = prof_dir {
                let _ = crate::keychain::activate_profile_token(&dir);
            }

            println!("Default profile set to '{}'.", target);
            Ok(())
        }
        None => {
            let reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
                eprintln!("Error: {}", e);
                1
            })?;
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
    let reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
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
    let isolated_home = crate::keychain::isolated_home_dir(&safe_profile_dir).map_err(|e| {
        eprintln!("Error: {e}");
        1
    })?;
    if fs::symlink_metadata(&isolated_home).is_ok() {
        crate::security::reject_symlink(&isolated_home).map_err(|e| {
            eprintln!("Refusing to delete unsafe isolated home: {e}");
            1
        })?;
        // remove_dir_all removes the mirrored HOME symlinks themselves, never their targets.
        fs::remove_dir_all(&isolated_home).map_err(|e| {
            eprintln!("Failed to delete isolated profile home: {e}");
            1
        })?;
    }
    fs::remove_dir_all(&safe_profile_dir).map_err(|e| {
        eprintln!("Failed to delete profile directory: {e}");
        1
    })?;
    ProfileRegistry::update(&config_path, |reg| reg.remove_profile(&profile.name)).map_err(
        |e| {
            eprintln!("Error saving registry: {}", e);
            1
        },
    )?;

    println!("Profile '{}' deleted.", profile.name);
    Ok(())
}
