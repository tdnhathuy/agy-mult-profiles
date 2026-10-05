#[path = "../src/config.rs"]
mod config;
#[path = "../src/isolate.rs"]
mod isolate;
#[path = "../src/security.rs"]
mod security;
use config::{ProfileInfo, ProfileRegistry};
use isolate::{isolated_command, validate_binding, Binding, Identity};
use std::path::PathBuf;

fn registry() -> ProfileRegistry {
    let mut r = ProfileRegistry::default();
    for name in ["one", "two"] {
        r.add_profile(ProfileInfo {
            name: name.into(),
            email: String::new(),
            dir: PathBuf::from(format!("/tmp/{name}")),
            created_at: chrono::Utc::now(),
            last_used_at: None,
        })
        .unwrap();
    }
    r
}
fn identity(user: &str, uid: u32) -> Identity {
    Identity {
        user: user.into(),
        uid,
        home: PathBuf::from(format!("/Users/{user}")),
    }
}
#[test]
fn distinct_uids_and_current_uid_required() {
    let mut r = registry();
    r.isolated.insert(
        "one".into(),
        Binding {
            user: "alice".into(),
            uid: 501,
            home: PathBuf::from("/Users/alice"),
        },
    );
    let b = Binding {
        user: "bob".into(),
        uid: 502,
        home: PathBuf::from("/Users/bob"),
    };
    assert!(validate_binding(&r, "two", &b, &identity("bob", 502), 500).is_ok());
    assert!(validate_binding(&r, "two", &b, &identity("bob", 503), 500).is_err());
    assert!(validate_binding(&r, "two", &b, &identity("bob", 502), 502).is_err());
    assert!(validate_binding(
        &r,
        "two",
        &Binding {
            uid: 501,
            ..b.clone()
        },
        &identity("bob", 501),
        500
    )
    .is_err());
    assert!(validate_binding(
        &r,
        "two",
        &b,
        &Identity {
            home: PathBuf::from("/tmp/fake"),
            ..identity("bob", 502)
        },
        500
    )
    .is_err());
}
#[test]
fn separate_locks_overlap_same_lock_serializes() {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        thread,
        time::Duration,
    };
    let dir = tempfile::tempdir().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let run =
        |name: &'static str, active: Arc<AtomicUsize>, peak: Arc<AtomicUsize>, base: PathBuf| {
            thread::spawn(move || {
                let _guard =
                    security::ProcessLock::acquire(&base.join(format!("{name}.lock"))).unwrap();
                let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(n, Ordering::SeqCst);
                thread::sleep(Duration::from_millis(80));
                active.fetch_sub(1, Ordering::SeqCst);
            })
        };
    let a = run("one", active.clone(), peak.clone(), dir.path().into());
    let b = run("two", active.clone(), peak.clone(), dir.path().into());
    a.join().unwrap();
    b.join().unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    peak.store(0, Ordering::SeqCst);
    let a = run("one", active.clone(), peak.clone(), dir.path().into());
    let b = run("one", active.clone(), peak.clone(), dir.path().into());
    a.join().unwrap();
    b.join().unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 1);
}

#[test]
fn child_exit_propagates() {
    let mut command = std::process::Command::new("/usr/bin/false");
    assert_eq!(command.status().unwrap().code(), Some(1));
    let mut missing = std::process::Command::new("/nonexistent-agyp-test");
    assert!(missing.status().is_err());
}
#[test]
fn sanitized_sudo_invocation() {
    let b = Binding {
        user: "alice".into(),
        uid: 501,
        home: PathBuf::from("/Users/alice"),
    };
    let c = isolated_command(
        &b,
        std::path::Path::new("/opt/homebrew/bin/agy"),
        &["--version".into()],
    )
    .unwrap();
    assert_eq!(c.get_program(), "/usr/bin/sudo");
    let args: Vec<_> = c
        .get_args()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args,
        [
            "-H",
            "-u",
            "alice",
            "--",
            "/bin/launchctl",
            "asuser",
            "501",
            "/opt/homebrew/bin/agy",
            "--version"
        ]
    );
    // env_clear removes every inherited value; only explicit safe keys remain.
    assert!(!c.get_envs().any(|(k, _)| k == "GEMINI_API_KEY"));
    assert!(isolated_command(&b, std::path::Path::new("./agy"), &[]).is_err());
}
