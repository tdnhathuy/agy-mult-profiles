#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub email: String,
    pub dir: PathBuf,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileRegistry {
    pub version: u32,
    pub default_profile: Option<String>,
    pub aliases: HashMap<String, String>,
    pub profiles: HashMap<String, ProfileInfo>,
}

impl Default for ProfileRegistry {
    fn default() -> Self {
        Self {
            version: 1,
            default_profile: None,
            aliases: HashMap::new(),
            profiles: HashMap::new(),
        }
    }
}

impl ProfileRegistry {
    pub fn get_profiles_dir() -> PathBuf {
        if let Ok(dir) = std::env::var("AGYP_PROFILES_DIR") {
            return PathBuf::from(dir);
        }
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        home.join(".gemini-profiles")
    }

    pub fn get_config_file_path() -> PathBuf {
        Self::get_profiles_dir().join("profiles.json")
    }

    pub fn load_or_default(path: &Path) -> Result<Self, String> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let content =
            fs::read_to_string(path).map_err(|e| format!("Failed to read registry: {}", e))?;
        serde_json::from_str(&content).map_err(|e| format!("Invalid JSON in registry: {}", e))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        if !parent.as_os_str().is_empty() {
            crate::security::secure_dir(parent)
                .map_err(|e| format!("Failed to create private parent dir: {}", e))?;
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("profiles.json");
        let tmp_path = if parent.as_os_str().is_empty() {
            PathBuf::from(format!("{}.tmp.{}", file_name, std::process::id()))
        } else {
            parent.join(format!("{}.tmp.{}", file_name, std::process::id()))
        };

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Serialization error: {}", e))?;

        let write_and_sync = || -> std::io::Result<()> {
            use std::io::Write;
            let mut options = fs::OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&tmp_path)?;
            file.write_all(content.as_bytes())?;
            file.sync_all()?;
            crate::security::set_private_file_permissions(&tmp_path)?;
            Ok(())
        };

        if let Err(e) = write_and_sync() {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("Failed to write temp registry file: {}", e));
        }

        if let Err(e) = fs::rename(&tmp_path, path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(format!("Failed to replace registry file: {}", e));
        }

        Ok(())
    }

    pub fn resolve_profile(&self, name_or_alias: &str) -> Option<&ProfileInfo> {
        if let Some(profile) = self.profiles.get(name_or_alias) {
            return Some(profile);
        }
        if let Some(target_name) = self.aliases.get(name_or_alias) {
            return self.profiles.get(target_name);
        }
        None
    }

    pub fn add_profile(&mut self, info: ProfileInfo) -> Result<(), String> {
        self.profiles.insert(info.name.clone(), info);
        Ok(())
    }

    pub fn remove_profile(&mut self, name: &str) -> Result<(), String> {
        if self.profiles.remove(name).is_none() {
            return Err(format!("Profile '{}' not found", name));
        }
        // Remove dangling aliases
        self.aliases.retain(|_, target| target != name);
        if self.default_profile.as_deref() == Some(name) {
            self.default_profile = self.profiles.keys().next().cloned();
        }
        Ok(())
    }

    pub fn set_alias(&mut self, alias: &str, profile_name: &str) -> Result<(), String> {
        if !self.profiles.contains_key(profile_name) {
            return Err(format!("Profile '{}' does not exist", profile_name));
        }
        self.aliases
            .insert(alias.to_string(), profile_name.to_string());
        Ok(())
    }

    pub fn remove_alias(&mut self, alias: &str) -> Result<(), String> {
        if self.aliases.remove(alias).is_none() {
            return Err(format!("Alias '{}' not found", alias));
        }
        Ok(())
    }

    pub fn set_default_profile(&mut self, profile_name: &str) -> Result<(), String> {
        let resolved_name = if self.profiles.contains_key(profile_name) {
            profile_name.to_string()
        } else if let Some(target) = self.aliases.get(profile_name) {
            target.clone()
        } else {
            return Err(format!(
                "Profile or alias '{}' does not exist",
                profile_name
            ));
        };
        self.default_profile = Some(resolved_name);
        Ok(())
    }
}
