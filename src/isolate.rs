#![allow(dead_code)]
use crate::config::ProfileRegistry;
use std::ffi::CString;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub user: String,
    pub uid: u32,
    pub home: PathBuf,
}
#[derive(Clone, Debug)]
pub struct Identity {
    pub user: String,
    pub uid: u32,
    pub home: PathBuf,
}

pub fn lookup(user: &str) -> Result<Identity, String> {
    if user.is_empty()
        || !user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err("Invalid local username".into());
    }
    let c = CString::new(user).map_err(|_| "Invalid username")?;
    unsafe {
        let p = libc::getpwnam(c.as_ptr());
        if p.is_null() {
            return Err("Local user not found".into());
        }
        let p = &*p;
        let actual = std::ffi::CStr::from_ptr(p.pw_name)
            .to_string_lossy()
            .into_owned();
        let home = PathBuf::from(
            std::ffi::CStr::from_ptr(p.pw_dir)
                .to_string_lossy()
                .into_owned(),
        );
        let id = Identity {
            user: actual,
            uid: p.pw_uid,
            home,
        };
        let ds = Command::new("/usr/bin/dscl")
            .args([
                ".",
                "-read",
                &format!("/Users/{user}"),
                "UniqueID",
                "NFSHomeDirectory",
            ])
            .output()
            .map_err(|_| "Local directory lookup failed")?;
        let text = String::from_utf8_lossy(&ds.stdout);
        if !ds.status.success()
            || !text
                .lines()
                .any(|l| l.trim() == format!("UniqueID: {}", id.uid))
            || !text
                .lines()
                .any(|l| l.trim() == format!("NFSHomeDirectory: {}", id.home.display()))
        {
            return Err("Not a matching local Directory Services account".into());
        }
        if id.user != user
            || id.uid < 500
            || !id.home.starts_with("/Users")
            || id.home.components().count() != 3
        {
            return Err("Not a standard local macOS user".into());
        }
        let meta = std::fs::symlink_metadata(&id.home).map_err(|_| "User home unavailable")?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || meta.uid() != id.uid
            || meta.mode() & 0o022 != 0
        {
            return Err("Unsafe user home ownership or permissions".into());
        }
        let state = id.home.join(".gemini");
        if state.exists() {
            let m =
                std::fs::symlink_metadata(&state).map_err(|_| "Cannot inspect target .gemini")?;
            if !m.is_dir()
                || m.file_type().is_symlink()
                || m.uid() != id.uid
                || m.mode() & 0o077 != 0
            {
                return Err("Unsafe target .gemini ownership or permissions".into());
            }
        }
        Ok(id)
    }
}
pub fn validate_binding(
    r: &ProfileRegistry,
    name: &str,
    b: &Binding,
    id: &Identity,
    current: u32,
) -> Result<(), String> {
    if !r.profiles.contains_key(name) {
        return Err("Unknown profile".into());
    }
    if b.user != id.user
        || b.uid != id.uid
        || b.home != id.home
        || b.uid == current
        || b.uid < 500
        || !b.home.starts_with("/Users")
        || b.home.components().count() != 3
    {
        return Err("Isolated identity mismatch".into());
    }
    if r.isolated.iter().any(|(n, x)| n != name && x.uid == b.uid) {
        return Err("UID already bound to another profile".into());
    }
    Ok(())
}

// Lock order: registry -> isolated profile locks (sorted). Exec releases registry
// after acquiring its profile lock and reading a fresh snapshot. No code holding
// a profile lock may reacquire registry. Writers hold both until save completes.
pub fn registry_lock(path: &Path) -> Result<crate::security::ProcessLock, String> {
    crate::security::ProcessLock::acquire(
        &path
            .parent()
            .ok_or("Invalid registry path")?
            .join("agyp-registry.lock"),
    )
}
pub fn isolated_exec_lock(dir: &Path, name: &str) -> Result<crate::security::ProcessLock, String> {
    crate::security::validate_component(name, "profile name")?;
    crate::security::ProcessLock::acquire(&dir.join(format!("isolated-{name}.lock")))
}
pub fn registry_update<T>(
    path: &Path,
    f: impl FnOnce(&mut ProfileRegistry) -> Result<T, String>,
) -> Result<T, String> {
    let _registry = registry_lock(path)?;
    let mut r = ProfileRegistry::load_or_default(path)?;
    // All isolated execution locks are held through the read/modify/write. This
    // prevents a changed alias/default/binding from rerouting active execution.
    let mut names: Vec<_> = r.profiles.keys().cloned().collect();
    names.extend(r.isolated.keys().cloned());
    names.sort();
    names.dedup();
    let _active: Vec<_> = names
        .iter()
        .map(|n| isolated_exec_lock(path.parent().unwrap(), n))
        .collect::<Result<_, _>>()?;
    let value = f(&mut r)?;
    r.save(path)?;
    Ok(value)
}

pub fn select_trusted_binary<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<PathBuf, String> {
    paths
        .into_iter()
        .find_map(|p| check_binary(p).ok())
        .ok_or_else(|| "No root-owned, non-writable agy binary found".into())
}
pub fn trusted_agy_binary() -> Result<PathBuf, String> {
    let mut paths: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).map(|d| d.join("agy")).collect())
        .unwrap_or_default();
    paths.extend([
        PathBuf::from("/opt/homebrew/bin/agy"),
        PathBuf::from("/usr/local/bin/agy"),
    ]);
    select_trusted_binary(paths.iter().map(PathBuf::as_path))
}

pub fn check_binary(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("agy binary must be absolute".into());
    }
    let resolved = path.canonicalize().map_err(|_| "agy binary unavailable")?;
    let mut p = Some(resolved.as_path());
    while let Some(item) = p {
        let m = std::fs::metadata(item).map_err(|_| "agy path unavailable")?;
        if m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err("Isolated agy path must be root-owned and not group/world writable".into());
        }
        p = item.parent();
    }
    if !resolved.is_file() {
        return Err("agy binary is not a file".into());
    }
    Ok(resolved)
}
pub fn isolated_command(b: &Binding, binary: &Path, args: &[String]) -> Result<Command, String> {
    if !binary.is_absolute()
        || b.user.is_empty()
        || !b
            .user
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return Err("Invalid isolated command".into());
    }
    let mut c = Command::new("/usr/bin/sudo");
    // A GUI bootstrap is necessary for the target user's login Keychain. The
    // caller checks gui/<uid> exists; sudo alone is not a login session.
    c.args([
        "-H",
        "-u",
        &b.user,
        "--",
        "/bin/launchctl",
        "asuser",
        &b.uid.to_string(),
    ]);
    c.arg(binary).args(args);
    c.env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", &b.home)
        .env("USER", &b.user)
        .env("LOGNAME", &b.user);
    Ok(c)
}
pub fn bind(profile: &str, user: &str) -> Result<(), String> {
    let path = ProfileRegistry::get_config_file_path();
    let id = lookup(user)?;
    registry_update(&path, |r| {
        let b = Binding {
            user: id.user.clone(),
            uid: id.uid,
            home: id.home.clone(),
        };
        validate_binding(r, profile, &b, &id, unsafe { libc::getuid() })?;
        r.isolated.insert(profile.into(), b);
        Ok(())
    })
}
pub fn unbind(profile: &str) -> Result<(), String> {
    registry_update(&ProfileRegistry::get_config_file_path(), |r| {
        if r.isolated.remove(profile).is_none() {
            return Err("Profile not bound".into());
        }
        Ok(())
    })
}
pub fn status(profile: Option<&str>) -> Result<(), String> {
    let r = ProfileRegistry::load_or_default(&ProfileRegistry::get_config_file_path())?;
    if let Some(name) = profile {
        if !r.profiles.contains_key(name) {
            return Err("Unknown profile".into());
        }
    }
    for (name, b) in &r.isolated {
        if profile.is_none_or(|p| p == name) {
            println!(
                "{name}: {} (uid {}, home {})",
                b.user,
                b.uid,
                b.home.display()
            );
        }
    }
    Ok(())
}

pub fn gui_session(uid: u32) -> Result<(), String> {
    let result = Command::new("/bin/launchctl")
        .args(["print", &format!("gui/{uid}")])
        .output()
        .map_err(|_| "Cannot check GUI session")?;
    if !result.status.success() {
        return Err(
            "Target user needs an active macOS GUI login session (login and unlock Keychain first)"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[test]
    fn skips_untrusted_path_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("agy");
        std::fs::write(&bad, "bad").unwrap();
        let selected = select_trusted_binary([bad.as_path(), std::path::Path::new("/bin/sh")]);
        assert_eq!(selected.unwrap(), std::path::PathBuf::from("/bin/sh"));
    }

    #[test]
    fn registry_writer_waits_for_isolated_exec_and_preserves_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.json");
        registry_update(&path, |r| {
            r.isolated.insert(
                "one".into(),
                Binding {
                    user: "test".into(),
                    uid: 501,
                    home: PathBuf::from("/Users/test"),
                },
            );
            Ok(())
        })
        .unwrap();
        let execution = isolated_exec_lock(dir.path(), "one").unwrap();
        let handle = std::thread::spawn({
            let path = path.clone();
            move || {
                registry_update(&path, |r| {
                    r.default_profile = Some("two".into());
                    Ok(())
                })
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!handle.is_finished());
        drop(execution);
        handle.join().unwrap().unwrap();
        assert_eq!(
            ProfileRegistry::load_or_default(&path)
                .unwrap()
                .default_profile
                .as_deref(),
            Some("two")
        );
    }
}
