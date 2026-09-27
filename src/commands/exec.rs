#![allow(dead_code)]

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

    let _keychain_guard = crate::keychain::KeychainGuard::new(&profile.dir).map_err(|e| {
        eprintln!(
            "Failed to configure keychain for profile '{}': {}",
            profile.name, e
        );
        1
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
