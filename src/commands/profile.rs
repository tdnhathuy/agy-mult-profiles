#![allow(dead_code)]

use chrono::Utc;
use serde_json::json;
use std::fs;
use std::io::{self, Write};

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
        let creds_file = dirs::home_dir().map(|h| h.join(".gemini/oauth_creds.json"));
        let creds_exist = creds_file.as_ref().map(|p| p.is_file()).unwrap_or(false);

        if !creds_exist {
            eprintln!("Warning: No active credentials found in ~/.gemini/oauth_creds.json. Please run 'agyp login <profile>' without --import-current to authenticate.");
        }

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
                eprintln!("Error saving registry: {}", e);
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

    let _ = fs::remove_dir_all(&profile.dir);
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
