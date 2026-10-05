#![allow(dead_code)]

use std::time::Instant;

use crate::agy::{build_agy_command, run_command_interruptibly};
use crate::config::ProfileRegistry;
use crate::keychain::{
    activate_profile, apply_profile_home, SystemKeychainBackend, PROFILE_KEYCHAIN_SETUP_TIMEOUT,
};

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

    let lock_path =
        crate::security::profile_lock_path(&ProfileRegistry::get_profiles_dir(), &profile.name);
    let process_lock = crate::security::ProcessLock::acquire_announcing(&lock_path, || {
        eprintln!(
            "Profile '{}' is in use by another agyp command; waiting...",
            profile.name
        )
    })
    .map_err(|e| {
        eprintln!("Failed to serialize AGY profile access: {e}");
        1
    })?;
    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let profile_dir = crate::security::validate_profile_dir_under(&profile.dir, &profiles_root)
        .map_err(|e| {
            eprintln!("Invalid profile directory: {e}");
            1
        })?;
    let (keychain_guard, profile_home) = activate_profile(
        &profile_dir,
        SystemKeychainBackend,
        Instant::now() + PROFILE_KEYCHAIN_SETUP_TIMEOUT,
        process_lock,
    )
    .map_err(|e| {
        eprintln!(
            "Failed to configure keychain for profile '{}': {}",
            profile.name, e
        );
        1
    })?;
    apply_profile_home(&mut cmd, profile_home.as_deref());

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
