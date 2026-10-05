#![allow(dead_code)]

use crate::agy::{build_agy_command, run_command_interruptibly};
use crate::config::ProfileRegistry;

pub fn cmd_exec(profile_arg: &str, raw_args: &[String]) -> Result<(), i32> {
    let config_path = ProfileRegistry::get_config_file_path();
    let _registry = crate::isolate::registry_lock(&config_path).map_err(|e| {
        eprintln!("Registry lock: {e}");
        1
    })?;
    let reg = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Error: {}", e);
        1
    })?;

    let profile = reg.resolve_profile(profile_arg).ok_or_else(|| {
        eprintln!("Error: Profile or alias '{}' not found.", profile_arg);
        1
    })?;

    if let Some(binding) = reg.isolated.get(&profile.name) {
        let _lock =
            crate::isolate::isolated_exec_lock(config_path.parent().unwrap(), &profile.name)
                .map_err(|e| {
                    eprintln!("Isolated lock: {e}");
                    1
                })?;
        drop(_registry);
        let id = crate::isolate::lookup(&binding.user).map_err(|e| {
            eprintln!("Identity: {e}");
            1
        })?;
        crate::isolate::validate_binding(&reg, &profile.name, binding, &id, unsafe {
            libc::getuid()
        })
        .map_err(|e| {
            eprintln!("Identity: {e}");
            1
        })?;
        crate::isolate::gui_session(binding.uid).map_err(|e| {
            eprintln!("Session: {e}");
            1
        })?;
        let binary = crate::isolate::trusted_agy_binary().map_err(|e| {
            eprintln!("Binary: {e}");
            1
        })?;
        let mut command =
            crate::isolate::isolated_command(binding, &binary, raw_args).map_err(|e| {
                eprintln!("Command: {e}");
                1
            })?;
        let status = crate::agy::run_command_isolated_interruptibly(&mut command).map_err(|e| {
            eprintln!("{e}");
            1
        })?;
        return if status.success() {
            Ok(())
        } else {
            Err(status.code().unwrap_or(1))
        };
    }

    let mut cmd = build_agy_command(&profile.dir, raw_args).map_err(|e| {
        eprintln!("Error: {}", e);
        127
    })?;

    // Legacy lock order is global AGY lock -> registry. Release the snapshot
    // lock before waiting, then recheck routing before touching shared Keychain.
    drop(_registry);
    let lock_path = ProfileRegistry::get_profiles_dir().join("agyp.lock");
    let _process_lock = crate::security::ProcessLock::acquire(&lock_path).map_err(|e| {
        eprintln!("Failed to serialize AGY profile access: {e}");
        1
    })?;
    let _registry = crate::isolate::registry_lock(&config_path).map_err(|e| {
        eprintln!("Registry lock: {e}");
        1
    })?;
    let current = ProfileRegistry::load_or_default(&config_path).map_err(|e| {
        eprintln!("Registry: {e}");
        1
    })?;
    if current.resolve_profile(profile_arg).map(|p| &p.name) != Some(&profile.name)
        || current.isolated.contains_key(&profile.name)
    {
        eprintln!("Profile routing changed; retry exec");
        return Err(1);
    }
    // Retain registry lock through Keychain restoration: bind/unbind must not
    // change routing during an in-flight legacy execution.
    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let profile_dir = crate::security::validate_profile_dir_under(&profile.dir, &profiles_root)
        .map_err(|e| {
            eprintln!("Invalid profile directory: {e}");
            1
        })?;
    let keychain_guard = crate::keychain::KeychainGuard::new(&profile_dir).map_err(|e| {
        eprintln!(
            "Failed to configure keychain for profile '{}': {}",
            profile.name, e
        );
        1
    })?;

    let status = run_command_interruptibly(&mut cmd).map_err(|e| {
        eprintln!("{e}");
        1
    })?;
    keychain_guard.sync_and_restore().map_err(|e| {
        eprintln!("Failed to persist or restore Keychain state: {e}");
        1
    })?;

    if status.success() {
        Ok(())
    } else {
        Err(status.code().unwrap_or(1))
    }
}
