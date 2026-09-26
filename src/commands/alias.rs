#![allow(dead_code)]

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
        println!(
            "{}",
            serde_json::to_string_pretty(&json!(reg.aliases)).unwrap()
        );
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
