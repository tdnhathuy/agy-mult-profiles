use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::tempdir;

#[path = "../src/keychain.rs"]
mod keychain;
#[path = "../src/security.rs"]
mod security;

use keychain::{
    activate_profile, isolated_home_dir, prepare_isolated_home_mirroring, KeychainBackend,
    MockKeychainBackend,
};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    orig: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, val: impl AsRef<std::ffi::OsStr>) -> Self {
        let orig = std::env::var_os(key);
        std::env::set_var(key, val);
        Self { key, orig }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.orig {
            Some(ref orig) => std::env::set_var(self.key, orig),
            None => std::env::remove_var(self.key),
        }
    }
}

/// Fake `security` that logs every call with its HOME, stores generic passwords per Keychain
/// file (`<keychain>.item`, or `system.item` when no Keychain is named), and reports the
/// search list relative to `$HOME` like the real tool. `MOCK_FOREIGN_KEYCHAIN` injects an
/// extra Keychain into the reported search list.
const MOCK_SECURITY: &str = r#"#!/bin/sh
MOCK_DIR="$(dirname "$0")"
echo "HOME=$HOME $*" >> "$MOCK_DIR/security.log"
cmd="$1"
shift
last=""
for a in "$@"; do last="$a"; done
case "$last" in
  /*) item="$last.item" ;;
  *) item="$MOCK_DIR/system.item" ;;
esac
case "$cmd" in
  find-generic-password)
    if [ -f "$item" ]; then cat "$item"; exit 0; fi
    echo "security: SecKeychainSearchCopyNext: The specified item could not be found in the keychain." >&2
    exit 44
    ;;
  add-generic-password)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "-w" ]; then printf "%s" "$2" > "$item"; exit 0; fi
      shift
    done
    exit 1
    ;;
  delete-generic-password)
    rm -f "$item"
    exit 0
    ;;
  create-keychain)
    : > "$last"
    exit 0
    ;;
  list-keychains)
    case " $* " in *" -s "*) exit 0 ;; esac
    echo "    \"$HOME/Library/Keychains/login.keychain-db\""
    if [ -n "$MOCK_FOREIGN_KEYCHAIN" ]; then echo "    \"$MOCK_FOREIGN_KEYCHAIN\""; fi
    echo "    \"/Library/Keychains/System.keychain\""
    exit 0
    ;;
  default-keychain)
    case " $* " in *" -s "*) exit 0 ;; esac
    echo "    \"$HOME/Library/Keychains/login.keychain-db\""
    exit 0
    ;;
  *)
    exit 0
    ;;
esac
"#;

struct Fixture {
    _temp: tempfile::TempDir,
    bin_dir: PathBuf,
    real_home: PathBuf,
    profile_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();

        let bin_dir = root.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let security_path = bin_dir.join("security");
        fs::write(&security_path, MOCK_SECURITY).unwrap();
        fs::set_permissions(&security_path, fs::Permissions::from_mode(0o755)).unwrap();

        let real_home = root.join("real-home");
        fs::create_dir_all(real_home.join(".config")).unwrap();
        fs::write(real_home.join(".gitconfig"), "[user]\n").unwrap();
        fs::create_dir_all(real_home.join("Library/Keychains")).unwrap();
        fs::write(
            real_home.join("Library/Keychains/login.keychain-db"),
            "real",
        )
        .unwrap();
        fs::create_dir_all(real_home.join("Library/Preferences")).unwrap();
        fs::write(real_home.join("Library/Preferences/app.plist"), "pref").unwrap();
        fs::create_dir_all(real_home.join("Library/Caches")).unwrap();

        let profile_dir = root.join("profiles-root/profiles/p1");
        fs::create_dir_all(&profile_dir).unwrap();

        Self {
            _temp: temp,
            bin_dir,
            real_home,
            profile_dir,
        }
    }

    fn path_guard(&self) -> EnvVarGuard {
        let path = format!(
            "{}:{}",
            self.bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        EnvVarGuard::set("PATH", path)
    }

    fn log(&self) -> String {
        fs::read_to_string(self.bin_dir.join("security.log")).unwrap_or_default()
    }
}

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}

#[test]
fn test_isolated_home_dir_lives_outside_profile_dir() {
    let home = isolated_home_dir(Path::new("/x/.gemini-profiles/profiles/work")).unwrap();
    assert_eq!(home, PathBuf::from("/x/.gemini-profiles/homes/work"));
}

#[test]
fn test_prepare_isolated_home_creates_private_keychain_and_mirrors_home() {
    let _lock = ENV_LOCK.lock().unwrap();
    let fx = Fixture::new();
    let _path = fx.path_guard();

    let isolated = prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline())
        .expect("isolated home should be prepared");

    let expected_home = fx
        .profile_dir
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("homes/p1");
    assert_eq!(isolated.home, expected_home);
    assert_eq!(
        isolated.keychain,
        expected_home.join("Library/Keychains/login.keychain-db")
    );
    assert!(
        isolated.keychain.is_file(),
        "profile Keychain must be created"
    );

    // User configuration is reachable through symlinks.
    assert_eq!(
        fs::read_link(expected_home.join(".gitconfig")).unwrap(),
        fx.real_home.join(".gitconfig")
    );
    assert_eq!(
        fs::read_link(expected_home.join(".config")).unwrap(),
        fx.real_home.join(".config")
    );
    assert_eq!(
        fs::read_link(expected_home.join("Library/Caches")).unwrap(),
        fx.real_home.join("Library/Caches")
    );

    // Library, Keychains and Preferences are private directories, never links to the real ones.
    for private in ["Library", "Library/Keychains", "Library/Preferences"] {
        let meta = fs::symlink_metadata(expected_home.join(private)).unwrap();
        assert!(meta.is_dir() && !meta.file_type().is_symlink(), "{private}");
    }
    assert!(!expected_home.join("Library/Preferences/app.plist").exists());
    assert_eq!(
        fs::read_to_string(fx.real_home.join("Library/Keychains/login.keychain-db")).unwrap(),
        "real"
    );

    let password_file = fx.profile_dir.join("keychain_password");
    let mode = fs::metadata(&password_file).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // Every security call runs with the isolated HOME.
    let log = fx.log();
    let home_prefix = format!("HOME={} ", expected_home.display());
    assert!(
        log.lines().all(|line| line.starts_with(&home_prefix)),
        "{log}"
    );
    assert!(log.contains("create-keychain"));
    assert!(log.contains(&format!(
        "list-keychains -d user -s {}",
        isolated.keychain.display()
    )));

    // Re-preparing reuses the Keychain and password but unlocks again.
    let password = fs::read_to_string(&password_file).unwrap();
    prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline()).unwrap();
    let log = fx.log();
    assert_eq!(log.matches("create-keychain").count(), 1);
    assert_eq!(log.matches("unlock-keychain").count(), 2);
    assert_eq!(fs::read_to_string(&password_file).unwrap(), password);
}

#[test]
fn test_prepare_isolated_home_rejects_foreign_keychain_in_search_list() {
    let _lock = ENV_LOCK.lock().unwrap();
    let fx = Fixture::new();
    let _path = fx.path_guard();
    let foreign = fx.real_home.join("Library/Keychains/login.keychain-db");
    let _foreign = EnvVarGuard::set("MOCK_FOREIGN_KEYCHAIN", &foreign);

    let err = prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline())
        .expect_err("a foreign Keychain in the search list must be refused");
    assert!(err.contains("unexpectedly includes"), "{err}");
}

#[test]
fn test_prepare_isolated_home_refuses_symlinked_library() {
    let _lock = ENV_LOCK.lock().unwrap();
    let fx = Fixture::new();
    let _path = fx.path_guard();
    let home = isolated_home_dir(&fx.profile_dir).unwrap();
    fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(fx.real_home.join("Library"), home.join("Library")).unwrap();
    let before = fs::metadata(fx.real_home.join("Library"))
        .unwrap()
        .permissions()
        .mode();

    let err = prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline())
        .expect_err("a symlinked Library must be refused");
    assert!(err.contains("symlink"), "{err}");
    let after = fs::metadata(fx.real_home.join("Library"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(before, after, "real Library permissions must not change");
}

#[test]
fn test_prepare_isolated_home_prunes_dangling_mirrors_and_adds_new_entries() {
    let _lock = ENV_LOCK.lock().unwrap();
    let fx = Fixture::new();
    let _path = fx.path_guard();
    fs::write(fx.real_home.join(".old"), "x").unwrap();

    let isolated =
        prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline()).unwrap();
    assert!(fs::symlink_metadata(isolated.home.join(".old")).is_ok());

    fs::remove_file(fx.real_home.join(".old")).unwrap();
    fs::write(fx.real_home.join(".new"), "y").unwrap();
    prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline()).unwrap();
    assert!(fs::symlink_metadata(isolated.home.join(".old")).is_err());
    assert_eq!(fs::read_to_string(isolated.home.join(".new")).unwrap(), "y");
}

#[test]
fn test_isolated_backend_only_touches_profile_keychain() {
    let _lock = ENV_LOCK.lock().unwrap();
    let fx = Fixture::new();
    let _path = fx.path_guard();
    fs::write(fx.bin_dir.join("system.item"), "system-token").unwrap();

    let isolated =
        prepare_isolated_home_mirroring(&fx.profile_dir, &fx.real_home, deadline()).unwrap();
    let backend = isolated.backend();

    assert_eq!(backend.read_token().unwrap(), None);
    backend.write_token("profile-token").unwrap();
    assert_eq!(
        backend.read_token().unwrap(),
        Some("profile-token".to_string())
    );
    backend.delete_token().unwrap();
    assert_eq!(backend.read_token().unwrap(), None);

    assert_eq!(
        fs::read_to_string(fx.bin_dir.join("system.item")).unwrap(),
        "system-token",
        "the shared Keychain item must be untouched"
    );
    let keychain = isolated.keychain.display().to_string();
    for line in fx.log().lines().filter(|l| l.contains("generic-password")) {
        assert!(
            line.ends_with(&keychain),
            "Keychain must be named explicitly: {line}"
        );
    }
}

#[test]
fn test_activate_profile_with_non_isolating_backend_uses_no_home() {
    let fx = Fixture::new();
    fs::write(fx.profile_dir.join("keychain_token.json"), "profile-token").unwrap();
    let backend = MockKeychainBackend::new(Some("system".to_string()));

    let (guard, home) = activate_profile(&fx.profile_dir, backend.clone(), deadline()).unwrap();
    assert!(home.is_none());
    assert_eq!(
        *backend.token.lock().unwrap(),
        Some("profile-token".to_string())
    );
    guard.sync_and_restore().unwrap();
    assert_eq!(*backend.token.lock().unwrap(), Some("system".to_string()));
    assert!(!isolated_home_dir(&fx.profile_dir).unwrap().exists());
}

#[test]
fn test_profile_locks_do_not_block_other_profiles() {
    let temp = tempdir().unwrap();
    let p1 = security::profile_lock_path(temp.path(), "p1");
    let p2 = security::profile_lock_path(temp.path(), "p2");
    assert_ne!(p1, p2);

    let _held = security::ProcessLock::acquire(&p1).unwrap();
    assert!(security::ProcessLock::acquire_with_timeout(&p2, Duration::from_millis(100)).is_ok());
    match security::ProcessLock::acquire_with_timeout(&p1, Duration::from_millis(100)) {
        Ok(_) => panic!("same profile must stay serialized"),
        Err(err) => assert!(err.contains("Timed out"), "{err}"),
    }
}

/// Exercises the real macOS `security` tool. Run explicitly with
/// `cargo test --test isolation_test -- --ignored real_security`.
#[test]
#[ignore]
fn test_real_security_isolated_keychain_round_trip() {
    let _lock = ENV_LOCK.lock().unwrap();
    let temp = tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let real_home = root.join("real-home");
    fs::create_dir_all(real_home.join("Library")).unwrap();
    let profile_dir = root.join("profiles-root/profiles/p1");
    fs::create_dir_all(&profile_dir).unwrap();

    let list_user_keychains = || {
        std::process::Command::new("/usr/bin/security")
            .args(["list-keychains", "-d", "user"])
            .output()
            .unwrap()
            .stdout
    };
    let before = list_user_keychains();

    let isolated = prepare_isolated_home_mirroring(&profile_dir, &real_home, deadline())
        .expect("real security must accept the isolated Keychain");
    let backend = isolated.backend();
    assert_eq!(backend.read_token().unwrap(), None);
    backend.write_token("profile-token").unwrap();
    assert_eq!(
        backend.read_token().unwrap(),
        Some("profile-token".to_string())
    );

    // An unqualified lookup under the isolated HOME (what AGY does) resolves to the profile item.
    let lookup = std::process::Command::new("/usr/bin/security")
        .env("HOME", &isolated.home)
        .args([
            "find-generic-password",
            "-s",
            "gemini",
            "-a",
            "antigravity",
            "-w",
        ])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&lookup.stdout).trim(),
        "profile-token"
    );

    // Re-preparing an existing Keychain must keep working (unlock + verify only).
    prepare_isolated_home_mirroring(&profile_dir, &real_home, deadline()).unwrap();
    backend.delete_token().unwrap();
    assert_eq!(backend.read_token().unwrap(), None);

    assert_eq!(
        String::from_utf8_lossy(&before),
        String::from_utf8_lossy(&list_user_keychains()),
        "the real user Keychain search list must not change"
    );
}
