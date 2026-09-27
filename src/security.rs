#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use std::thread;
use std::time::Duration;

use fs2::FileExt;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_USERS: OnceLock<Mutex<usize>> = OnceLock::new();

extern "C" fn handle_termination_signal(_signal: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn signal_users() -> &'static Mutex<usize> {
    SIGNAL_USERS.get_or_init(|| Mutex::new(0))
}

struct SignalGuard;

impl SignalGuard {
    fn install() -> Result<Self, String> {
        INTERRUPTED.store(false, Ordering::SeqCst);
        let mut users = signal_users()
            .lock()
            .map_err(|_| "Signal-handler state is poisoned".to_string())?;
        if *users == 0 {
            unsafe {
                if libc::signal(
                    libc::SIGINT,
                    handle_termination_signal as *const () as libc::sighandler_t,
                ) == libc::SIG_ERR
                    || libc::signal(
                        libc::SIGTERM,
                        handle_termination_signal as *const () as libc::sighandler_t,
                    ) == libc::SIG_ERR
                    || libc::signal(
                        libc::SIGHUP,
                        handle_termination_signal as *const () as libc::sighandler_t,
                    ) == libc::SIG_ERR
                {
                    return Err("Failed to install termination signal handlers".to_string());
                }
            }
        }
        *users += 1;
        Ok(Self)
    }
}

impl Drop for SignalGuard {
    fn drop(&mut self) {
        if let Ok(mut users) = signal_users().lock() {
            *users = users.saturating_sub(1);
            if *users == 0 {
                unsafe {
                    libc::signal(libc::SIGINT, libc::SIG_DFL);
                    libc::signal(libc::SIGTERM, libc::SIG_DFL);
                    libc::signal(libc::SIGHUP, libc::SIG_DFL);
                }
            }
        }
    }
}

pub struct ProcessLock {
    file: File,
    _signals: SignalGuard,
}

impl ProcessLock {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            secure_dir(parent).map_err(|e| format!("Failed to secure lock directory: {e}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("Failed to open process lock: {e}"))?;
        set_private_file_permissions(path)
            .map_err(|e| format!("Failed to secure process lock: {e}"))?;
        file.lock_exclusive()
            .map_err(|e| format!("Failed to acquire process lock: {e}"))?;
        Ok(Self {
            file,
            _signals: SignalGuard::install()?,
        })
    }

    pub fn acquire_with_timeout(path: &Path, timeout: Duration) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            secure_dir(parent).map_err(|e| format!("Failed to secure lock directory: {e}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("Failed to open process lock: {e}"))?;
        set_private_file_permissions(path)
            .map_err(|e| format!("Failed to secure process lock: {e}"))?;

        let start = std::time::Instant::now();
        loop {
            if Self::interrupted() {
                return Err("Interrupted while waiting for process lock".to_string());
            }

            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(err) => {
                    let would_block = err.kind() == io::ErrorKind::WouldBlock
                        || err.raw_os_error() == Some(libc::EWOULDBLOCK)
                        || err.raw_os_error() == Some(libc::EAGAIN);
                    if !would_block {
                        return Err(format!("Failed to acquire process lock: {err}"));
                    }
                }
            }

            if start.elapsed() >= timeout {
                return Err(format!(
                    "Timed out waiting for process lock after {:?}",
                    timeout
                ));
            }

            thread::sleep(Duration::from_millis(25));
        }

        Ok(Self {
            file,
            _signals: SignalGuard::install()?,
        })
    }

    pub fn interrupted() -> bool {
        INTERRUPTED.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub fn mark_interrupted_for_test() {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

pub fn secure_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub fn set_private_file_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    reject_symlink(path)?;
    if let Some(parent) = path.parent() {
        secure_dir(parent)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true).mode(0o600);
        let mut file = options.open(path)?;
        use std::io::Write;
        file.write_all(contents)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        fs::write(path, contents)?;
    }
    set_private_file_permissions(path)
}

pub fn reject_symlink(path: &Path) -> io::Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("refusing to access symlink: {}", path.display()),
            ));
        }
    }
    Ok(())
}

pub fn validate_component(value: &str, kind: &str) -> Result<(), String> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.len() > 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(format!(
            "Invalid {kind} '{value}'. Use 1-64 ASCII letters, digits, '-' or '_'."
        ));
    }
    Ok(())
}

pub fn profile_root() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gemini-profiles")
        .join("profiles")
}

pub fn validate_profile_dir(path: &Path) -> Result<PathBuf, String> {
    validate_profile_dir_under(path, &profile_root())
}

pub fn validate_profile_dir_under(path: &Path, root: &Path) -> Result<PathBuf, String> {
    let canonical_root = root
        .canonicalize()
        .map_err(|e| format!("Failed to resolve profiles root: {e}"))?;
    let canonical_path = path
        .canonicalize()
        .map_err(|e| format!("Failed to resolve profile directory: {e}"))?;
    if canonical_path.parent() != Some(canonical_root.as_path()) || canonical_path == canonical_root
    {
        return Err(format!(
            "Profile directory escapes the managed profiles root: {}",
            path.display()
        ));
    }
    Ok(canonical_path)
}
