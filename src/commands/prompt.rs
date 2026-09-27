#![allow(dead_code)]

use chrono::Utc;
use serde_json::json;
use std::io::{self, Read};

use crate::agy::{build_agy_command, check_profile_auth};
use crate::config::ProfileRegistry;

fn print_json_error(profile: &str, email: Option<&str>, exit_code: i32, error_msg: &str) {
    let result = json!({
        "profile": profile,
        "email": email,
        "status": "error",
        "exit_code": exit_code,
        "output": "",
        "error": error_msg,
    });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}

fn profile_email_opt(email: &str) -> Option<&str> {
    if email.is_empty() || email == "unknown" {
        None
    } else {
        Some(email)
    }
}

pub fn cmd_prompt(
    profile_arg: Option<&str>,
    prompt_text_arg: Option<&str>,
    model: Option<&str>,
    add_dirs: &[String],
    no_skip_perm: bool,
    json_output: bool,
) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let mut reg = match ProfileRegistry::load_or_default(&config_path) {
        Ok(r) => r,
        Err(e) => {
            let target = profile_arg.unwrap_or("unknown");
            let err_msg = format!("Failed to load registry: {}", e);
            if json_output {
                print_json_error(target, None, 1, &err_msg);
            } else {
                eprintln!("Error: {}", err_msg);
            }
            return Err(1);
        }
    };

    let target_profile_key = match profile_arg {
        Some(p) => p.to_string(),
        None => match reg.default_profile.clone() {
            Some(p) => p,
            None => {
                let err_msg = "No profile specified and no default profile set. Run 'agyp default <profile>' or 'agyp prompt <profile>'.";
                if json_output {
                    print_json_error("unknown", None, 1, err_msg);
                } else {
                    eprintln!("Error: {}", err_msg);
                }
                return Err(1);
            }
        },
    };

    let profile = match reg.resolve_profile(&target_profile_key) {
        Some(p) => p.clone(),
        None => {
            let err_msg = format!("Profile or alias '{}' not found.", target_profile_key);
            if json_output {
                print_json_error(&target_profile_key, None, 1, &err_msg);
            } else {
                eprintln!("Error: {}", err_msg);
            }
            return Err(1);
        }
    };

    if !check_profile_auth(&profile.dir) {
        let err_msg = format!(
            "Profile '{}' is not authenticated. Please run 'agyp login {}' first.",
            profile.name, profile.name
        );
        if json_output {
            print_json_error(
                &profile.name,
                profile_email_opt(&profile.email),
                2,
                &err_msg,
            );
        } else {
            eprintln!("Error: {}", err_msg);
        }
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
                let err_msg = "Prompt content cannot be empty.";
                if json_output {
                    print_json_error(&profile.name, profile_email_opt(&profile.email), 1, err_msg);
                } else {
                    eprintln!("Error: {}", err_msg);
                }
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

    let mut cmd = match build_agy_command(&profile.dir, &args) {
        Ok(c) => c,
        Err(e) => {
            if json_output {
                print_json_error(&profile.name, profile_email_opt(&profile.email), 127, &e);
            } else {
                eprintln!("Error: {}", e);
            }
            return Err(127);
        }
    };

    // Activate target profile credentials in macOS Keychain for agy execution
    let _keychain_guard = crate::keychain::KeychainGuard::new(&profile.dir).map_err(|e| {
        let err_msg = format!(
            "Failed to configure keychain for profile '{}': {}",
            profile.name, e
        );
        if json_output {
            print_json_error(
                &profile.name,
                profile_email_opt(&profile.email),
                1,
                &err_msg,
            );
        } else {
            eprintln!("Error: {}", err_msg);
        }
        1
    })?;

    if json_output {
        let output = match cmd.output() {
            Ok(o) => o,
            Err(e) => {
                let err_msg = format!("Failed to execute agy: {}", e);
                print_json_error(
                    &profile.name,
                    profile_email_opt(&profile.email),
                    1,
                    &err_msg,
                );
                return Err(1);
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(1);

        let result = json!({
            "profile": profile.name,
            "email": profile.email,
            "status": if exit_code == 0 { "success" } else { "error" },
            "exit_code": exit_code,
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
