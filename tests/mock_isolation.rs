//! Mock-only experiment; must not be used as a production authentication backend.
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use agyp::security;

pub fn prepare_mock_profile(root: &Path, name: &str, fake_token: &str) -> Result<PathBuf, String> {
    security::validate_component(name, "profile")?;
    let home = root.join(name);
    if home.exists() || home.symlink_metadata().is_ok() {
        return Err("Refusing to reuse existing mock HOME".into());
    }
    security::secure_dir(&home).map_err(|_| "Failed to secure mock HOME")?;
    let gemini = home.join(".gemini");
    security::secure_dir(&gemini).map_err(|_| "Failed to secure mock profile")?;
    if gemini.canonicalize().map_err(|_| "Invalid mock profile")?
        != home
            .canonicalize()
            .map_err(|_| "Invalid mock HOME")?
            .join(".gemini")
    {
        return Err("Mock profile escaped HOME".into());
    }
    security::write_private_file(
        &gemini.join("jetski-standalone-oauth-token"),
        fake_token.as_bytes(),
    )
    .map_err(|_| "Failed to write fake token")?;
    // Explicitly do not link or create a Keychain directory.
    Ok(home)
}

pub fn run_mock(
    script: &Path,
    home: &Path,
    expected: &str,
    events: &Path,
    exit_code: &str,
    pause: &str,
) -> Result<Child, String> {
    let gemini = home.join(".gemini");
    if gemini
        .symlink_metadata()
        .map_err(|_| "Missing mock profile")?
        .file_type()
        .is_symlink()
        || !gemini.is_dir()
    {
        return Err("Unsafe mock profile".into());
    }
    let token = gemini.join("jetski-standalone-oauth-token");
    if token
        .symlink_metadata()
        .map_err(|_| "Missing fake token")?
        .file_type()
        .is_symlink()
    {
        return Err("Unsafe fake token".into());
    }
    // Fake script appends/writes its own test state, never exposes credential bytes to stdout.
    let mut cmd = Command::new(script);
    cmd.arg(format!("--gemini_dir={}", gemini.display()))
        .env("HOME", home)
        .env("SSH_CONNECTION", "127.0.0.1 50000 127.0.0.1 22")
        .env("SSH_CLIENT", "127.0.0.1 50000 22")
        .env("EXPECTED", expected)
        .env("EVENTS", events)
        .env("EXIT_CODE", exit_code)
        .env("PAUSE", pause)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().map_err(|_| "Failed to start mock CLI".into())
}
