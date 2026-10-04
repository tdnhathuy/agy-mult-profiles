//! Deliberately non-executing sandbox preparation. No imported token is trusted.
use crate::config::ProfileRegistry;
use crate::security::validate_component;
use std::fs;
use std::path::Path;

fn new_private_dir(path: &Path) -> Result<(), String> {
    // create_dir (not create_dir_all) refuses an existing path including a symlink.
    fs::create_dir(path)
        .map_err(|_| "sandbox path already exists or cannot be created".to_string())?;
    crate::security::secure_dir(path).map_err(|_| "cannot secure sandbox directory".to_string())
}

pub fn prepare(profile: &str) -> Result<(), String> {
    validate_component(profile, "profile")?;
    let root = ProfileRegistry::get_profiles_dir();
    let meta = fs::symlink_metadata(&root).map_err(|_| "profiles root is missing".to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("profiles root must be a real directory".to_string());
    }
    let probe_root = root.join("isolated-probe");
    match fs::symlink_metadata(&probe_root) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            return Err("probe root must be a real directory".to_string())
        }
        Ok(_) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => new_private_dir(&probe_root)?,
        Err(_) => return Err("cannot inspect probe root".to_string()),
    }
    new_private_dir(&probe_root.join(profile))?;
    let home = probe_root.join(profile).join("home");
    new_private_dir(&home)?;
    new_private_dir(&home.join(".gemini"))?;
    println!("Private empty sandbox prepared for {profile}; not authenticated. agyp exec remains serialized through Keychain. Do not copy profile tokens into this sandbox.");
    Ok(())
}
