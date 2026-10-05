#![allow(dead_code)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

pub fn locate_agy_binary() -> Result<PathBuf, String> {
    // 1. Check PATH
    if let Ok(path_var) = env::var("PATH") {
        for p in env::split_paths(&path_var) {
            let candidate = p.join("agy");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    // 2. Check standard locations
    if let Some(home) = dirs::home_dir() {
        let local_bin = home.join(".local/bin/agy");
        if local_bin.is_file() {
            return Ok(local_bin);
        }
    }

    let brew_bin = PathBuf::from("/opt/homebrew/bin/agy");
    if brew_bin.is_file() {
        return Ok(brew_bin);
    }

    let usr_local_bin = PathBuf::from("/usr/local/bin/agy");
    if usr_local_bin.is_file() {
        return Ok(usr_local_bin);
    }

    Err(
        "Could not find 'agy' binary in PATH or standard paths (~/.local/bin/agy, /opt/homebrew/bin/agy)."
            .to_string(),
    )
}

pub fn check_profile_auth(profile_dir: &Path) -> bool {
    profile_dir.join("keychain_token.json").is_file()
        || profile_dir.join("oauth_creds.json").is_file()
        || profile_dir.join("jetski-standalone-oauth-token").is_file()
}

pub fn extract_profile_email(profile_dir: &Path) -> Option<String> {
    // 1. Try keychain_token.json
    let keychain_file = profile_dir.join("keychain_token.json");
    if keychain_file.is_file() {
        if let Ok(content) = fs::read_to_string(&keychain_file) {
            if let Some(email) = crate::keychain::extract_email_from_token_string(&content) {
                return Some(email);
            }
        }
    }

    // 2. Try jetski-standalone-oauth-token
    let standalone_file = profile_dir.join("jetski-standalone-oauth-token");
    if standalone_file.is_file() {
        if let Ok(content) = fs::read_to_string(&standalone_file) {
            if let Some(email) = crate::keychain::extract_email_from_token_string(&content) {
                return Some(email);
            }
        }
    }

    // 3. Try oauth_creds.json
    let creds_file = profile_dir.join("oauth_creds.json");
    if creds_file.is_file() {
        if let Ok(content) = fs::read_to_string(&creds_file) {
            if let Some(email) = crate::keychain::extract_email_from_token_string(&content) {
                return Some(email);
            }
        }
    }

    // 4. Try google_accounts.json
    let accounts_file = profile_dir.join("google_accounts.json");
    if accounts_file.is_file() {
        if let Ok(content) = fs::read_to_string(&accounts_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(active) = val.get("active").and_then(|v| v.as_str()) {
                    return Some(active.to_string());
                }
            }
        }
    }
    None
}

pub fn build_agy_command(profile_dir: &Path, extra_args: &[String]) -> Result<Command, String> {
    let agy_bin = locate_agy_binary()?;
    let mut cmd = Command::new(agy_bin);
    cmd.arg(format!("--gemini_dir={}", profile_dir.display()));
    for arg in extra_args {
        cmd.arg(arg);
    }
    Ok(cmd)
}

#[derive(Debug, Clone)]
pub struct CapturedOutput {
    pub status: Option<ExitStatus>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

fn bounded_drain_readers(
    stdout_done: &std::sync::atomic::AtomicBool,
    stderr_done: &std::sync::atomic::AtomicBool,
    mut stdout_handle: Option<thread::JoinHandle<()>>,
    mut stderr_handle: Option<thread::JoinHandle<()>>,
    drain_limit: Duration,
) {
    let start = std::time::Instant::now();
    while start.elapsed() < drain_limit {
        let out_f = stdout_done.load(std::sync::atomic::Ordering::SeqCst);
        let err_f = stderr_done.load(std::sync::atomic::Ordering::SeqCst);
        if out_f && err_f {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    // Only join threads that have confirmed completion!
    // Never do an unbounded join on a thread that may be blocked in read().
    if stdout_done.load(std::sync::atomic::Ordering::SeqCst) {
        if let Some(h) = stdout_handle.take() {
            let _ = h.join();
        }
    }
    if stderr_done.load(std::sync::atomic::Ordering::SeqCst) {
        if let Some(h) = stderr_handle.take() {
            let _ = h.join();
        }
    }
    // Any uncompleted handles are dropped here, detaching the thread safely.
}

pub fn run_command_captured_interruptibly(cmd: &mut Command) -> Result<CapturedOutput, String> {
    run_command_captured_interruptibly_with_timeout(cmd, None)
}

pub fn run_command_captured_interruptibly_with_timeout(
    cmd: &mut Command,
    timeout: Option<Duration>,
) -> Result<CapturedOutput, String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute agy: {e}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let stderr_buf = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let stdout_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stderr_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    let out_clone = std::sync::Arc::clone(&stdout_buf);
    let out_done_clone = std::sync::Arc::clone(&stdout_done);
    let stdout_handle = thread::spawn(move || {
        if let Some(mut r) = stdout {
            let mut chunk = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut r, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut lock) = out_clone.lock() {
                            lock.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        out_done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let err_clone = std::sync::Arc::clone(&stderr_buf);
    let err_done_clone = std::sync::Arc::clone(&stderr_done);
    let stderr_handle = thread::spawn(move || {
        if let Some(mut r) = stderr {
            let mut chunk = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut r, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        if let Ok(mut lock) = err_clone.lock() {
                            lock.extend_from_slice(&chunk[..n]);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        }
        err_done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
    });

    let mut stdout_handle = Some(stdout_handle);
    let mut stderr_handle = Some(stderr_handle);

    let start_time = std::time::Instant::now();
    let mut timed_out = false;
    let mut exit_status: Option<ExitStatus> = None;
    let child_pid = child.id();

    const TERMINATION_DRAIN_LIMIT: Duration = Duration::from_millis(100);

    loop {
        if crate::security::ProcessLock::interrupted() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child_pid as libc::pid_t), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            bounded_drain_readers(
                &stdout_done,
                &stderr_done,
                stdout_handle.take(),
                stderr_handle.take(),
                TERMINATION_DRAIN_LIMIT,
            );
            return Err("Interrupted while AGY was running; credentials were restored".to_string());
        }

        if let Some(limit) = timeout {
            if start_time.elapsed() >= limit {
                timed_out = true;
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child_pid as libc::pid_t), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                bounded_drain_readers(
                    &stdout_done,
                    &stderr_done,
                    stdout_handle.take(),
                    stderr_handle.take(),
                    TERMINATION_DRAIN_LIMIT,
                );
                break;
            }
        }

        if exit_status.is_none() {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| format!("Failed to wait for agy: {e}"))?
            {
                exit_status = Some(status);
            }
        }

        let stdout_finished = stdout_done.load(std::sync::atomic::Ordering::SeqCst);
        let stderr_finished = stderr_done.load(std::sync::atomic::Ordering::SeqCst);

        if exit_status.is_some() && stdout_finished && stderr_finished {
            if let Some(h) = stdout_handle.take() {
                let _ = h.join();
            }
            if let Some(h) = stderr_handle.take() {
                let _ = h.join();
            }
            break;
        }

        thread::sleep(Duration::from_millis(20));
    }

    let stdout_bytes = stdout_buf.lock().map(|b| b.clone()).unwrap_or_default();
    let stderr_bytes = stderr_buf.lock().map(|b| b.clone()).unwrap_or_default();

    Ok(CapturedOutput {
        status: exit_status,
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        timed_out,
    })
}

pub fn run_command_isolated_with_interrupt(
    cmd: &mut Command,
    interrupted: impl Fn() -> bool,
) -> Result<ExitStatus, String> {
    use std::os::unix::process::CommandExt;
    // Separate group ensures sudo, launchctl and agy share a termination target.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute agy: {e}"))?;
    let group = child.id() as i32;
    loop {
        if interrupted() {
            unsafe {
                libc::kill(-group, libc::SIGTERM);
            }
            let until = std::time::Instant::now() + Duration::from_millis(500);
            // Keep leader unreaped so group ID cannot be recycled before teardown.
            while std::time::Instant::now() < until {
                if unsafe { libc::kill(-group, 0) } == -1 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if unsafe { libc::kill(-group, 0) } == 0 {
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
            }
            child.wait().map_err(|e| e.to_string())?;
            return Err("Interrupted while isolated AGY was running".into());
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(50));
    }
}
pub fn run_command_isolated_interruptibly(cmd: &mut Command) -> Result<ExitStatus, String> {
    run_command_isolated_with_interrupt(cmd, crate::security::ProcessLock::interrupted)
}

pub fn run_command_interruptibly(cmd: &mut Command) -> Result<ExitStatus, String> {
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to execute agy: {e}"))?;

    loop {
        if crate::security::ProcessLock::interrupted() {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Interrupted while AGY was running; credentials were restored".to_string());
        }
        match child
            .try_wait()
            .map_err(|e| format!("Failed to wait for agy: {e}"))?
        {
            Some(status) => return Ok(status),
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
mod isolated_regression {
    use super::*;
    #[test]
    fn interrupt_reaps_process_group_descendants() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survivor");
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c")
            .arg("(sleep 2; touch \"$1\") & wait")
            .arg("sh")
            .arg(&marker);
        let interrupted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = interrupted.clone();
        let signal = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let result = run_command_isolated_with_interrupt(&mut cmd, || {
            interrupted.load(std::sync::atomic::Ordering::SeqCst)
        });
        signal.join().unwrap();
        assert!(result.is_err());
        std::thread::sleep(Duration::from_secs(3));
        assert!(!marker.exists(), "descendant survived interruption");
    }
}
