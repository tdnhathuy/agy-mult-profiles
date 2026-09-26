#![allow(dead_code)]

use chrono::Utc;
use serde_json::json;
use std::io::{self, Read};

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

    let profile = reg
        .resolve_profile(&target_profile_key)
        .ok_or_else(|| {
            eprintln!(
                "Error: Profile or alias '{}' not found.",
                target_profile_key
            );
            1
        })?
        .clone();

    if !check_profile_auth(&profile.dir) {
        eprintln!(
            "Error: Profile '{}' is not authenticated. Please run 'agyp login {}' first.",
            profile.name, profile.name
        );
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
    if let Some(p) = reg.profiles.get_mut(&profile.name) {
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
        return if exit_code == 0 {
            Ok(())
        } else {
            Err(exit_code)
        };
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
