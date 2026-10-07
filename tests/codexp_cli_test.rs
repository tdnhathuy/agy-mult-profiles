//! End-to-end codexp runs against a fake `codex` binary in a temporary profiles dir.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::{json, Value};
use tempfile::TempDir;

fn jwt(payload: Value) -> String {
    format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(payload.to_string()))
}

fn write_export(dir: &Path, email: &str, account: &str, exp_offset: i64) -> PathBuf {
    let exp = chrono::Utc::now().timestamp() + exp_offset;
    let auth = json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt(json!({"email": email, "https://api.openai.com/auth": {"chatgpt_account_id": account, "chatgpt_plan_type": "plus"}})),
            "access_token": jwt(json!({"exp": exp, "https://api.openai.com/profile": {"email": email}, "https://api.openai.com/auth": {"chatgpt_account_id": account}})),
            "refresh_token": "upstream-secret",
            "account_id": account
        },
        "last_refresh": "2026-10-06T07:10:00.000Z"
    });
    let path = dir.join(format!("auth-{email}.json"));
    fs::write(&path, auth.to_string()).unwrap();
    path
}

struct Env {
    tmp: TempDir,
}

impl Env {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        // Fake codex: records its args and env, then emits a successful exec event stream.
        let fake = tmp.path().join("fake-codex");
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{log}/args'\nenv > '{log}/env'\npwd > '{log}/cwd'\n\
                 echo '{{\"type\":\"thread.started\",\"thread_id\":\"t-1\"}}'\n\
                 echo '{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"all done\"}}}}'\n\
                 echo '{{\"type\":\"turn.completed\",\"usage\":{{\"output_tokens\":3}}}}'\n",
                log = tmp.path().display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        Self { tmp }
    }

    fn profiles(&self) -> PathBuf {
        self.tmp.path().join("profiles")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codexp"))
            .args(args)
            .env("CODEXP_PROFILES_DIR", self.profiles())
            .env("CODEXP_CODEX_BIN", self.tmp.path().join("fake-codex"))
            .env("OPENAI_API_KEY", "should-be-stripped")
            .output()
            .unwrap()
    }

    fn linked_worktree(&self) -> PathBuf {
        let repo = self.tmp.path().join("repo");
        let wt = self.tmp.path().join("wt");
        let git = |args: &[&str], dir: &Path| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                ok.status.success(),
                "{}",
                String::from_utf8_lossy(&ok.stderr)
            );
        };
        fs::create_dir_all(&repo).unwrap();
        git(&["init", "-q"], &repo);
        git(
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "init",
            ],
            &repo,
        );
        git(&["worktree", "add", "-q", wt.to_str().unwrap()], &repo);
        wt
    }
}

#[test]
fn import_list_and_task_use_isolated_home_without_refresh() {
    let env = Env::new();
    let export = write_export(env.tmp.path(), "a@x.com", "acct-a", 7 * 86_400);

    let out = env.run(&[
        "import",
        "c1",
        "--from",
        export.to_str().unwrap(),
        "--remove-source",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!export.exists(), "--remove-source deletes the export");

    let home = env.profiles().join("homes/c1");
    let auth = fs::read_to_string(home.join("auth.json")).unwrap();
    assert!(!auth.contains("upstream-secret"));
    assert_eq!(
        fs::metadata(home.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(fs::symlink_metadata(home.join("config.toml"))
        .unwrap()
        .file_type()
        .is_symlink());

    let list = env.run(&["list", "--json"]);
    let rows: Value = serde_json::from_slice(&list.stdout).unwrap();
    assert_eq!(rows[0]["name"], "c1");
    assert_eq!(rows[0]["email"], "a@x.com");
    assert_eq!(rows[0]["status"], "ready");

    let wt = env.linked_worktree();
    let prompt = env.tmp.path().join("prompt.md");
    fs::write(&prompt, "-starts with a dash").unwrap();
    let task = env.run(&[
        "task",
        "--profile",
        "c1",
        "--worktree",
        wt.to_str().unwrap(),
        "--prompt-file",
        prompt.to_str().unwrap(),
        "--json",
    ]);
    let result: Value = serde_json::from_slice(&task.stdout).unwrap();
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(result["final_message"], "all done");
    assert_eq!(result["thread_id"], "t-1");
    assert_eq!(result["email"], "a@x.com");

    let logged_env = fs::read_to_string(env.tmp.path().join("env")).unwrap();
    assert!(logged_env.contains(&format!("CODEX_HOME={}", home.display())));
    assert!(logged_env.contains("CODEX_REFRESH_TOKEN_URL_OVERRIDE=http://127.0.0.1:9/"));
    assert!(!logged_env.contains("OPENAI_API_KEY"));
    let args = fs::read_to_string(env.tmp.path().join("args")).unwrap();
    assert!(args.starts_with("exec\n"));
    assert!(args.contains("model_provider=\"openai\""));
    assert!(args.contains("sandbox_mode=\"workspace-write\""));
    assert!(args.ends_with("--\n-starts with a dash\n"), "{args}");
    let cwd = fs::read_to_string(env.tmp.path().join("cwd")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        wt.canonicalize().unwrap()
    );
}

#[test]
fn import_refuses_expired_tokens_and_account_swaps() {
    let env = Env::new();
    let expired = write_export(env.tmp.path(), "old@x.com", "acct-o", -60);
    assert!(!env
        .run(&["import", "c1", "--from", expired.to_str().unwrap()])
        .status
        .success());

    let a = write_export(env.tmp.path(), "a@x.com", "acct-a", 86_400 * 3);
    let b = write_export(env.tmp.path(), "b@x.com", "acct-b", 86_400 * 3);
    assert!(env
        .run(&["import", "c1", "--from", a.to_str().unwrap()])
        .status
        .success());
    // Re-importing the same account refreshes it.
    assert!(env
        .run(&["import", "c1", "--from", a.to_str().unwrap()])
        .status
        .success());
    // A different account into the same profile, or the same account twice, needs --force.
    assert!(!env
        .run(&["import", "c1", "--from", b.to_str().unwrap()])
        .status
        .success());
    assert!(!env
        .run(&["import", "c2", "--from", a.to_str().unwrap()])
        .status
        .success());
    assert!(env
        .run(&["import", "c1", "--from", b.to_str().unwrap(), "--force"])
        .status
        .success());
}

#[test]
fn task_with_expired_profile_exits_unauthenticated() {
    let env = Env::new();
    let soon = write_export(env.tmp.path(), "a@x.com", "acct-a", 3600);
    assert!(env
        .run(&["import", "c1", "--from", soon.to_str().unwrap()])
        .status
        .success());
    // Simulate expiry by rewriting the stored token.
    let home = env.profiles().join("homes/c1/auth.json");
    let mut auth: Value = serde_json::from_str(&fs::read_to_string(&home).unwrap()).unwrap();
    auth["tokens"]["access_token"] = json!(jwt(
        json!({"exp": 1, "https://api.openai.com/profile": {"email": "a@x.com"}})
    ));
    fs::write(&home, auth.to_string()).unwrap();

    let wt = env.linked_worktree();
    let out = env.run(&[
        "task",
        "--profile",
        "c1",
        "--worktree",
        wt.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(2));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["failure_kind"], "unauthenticated");
    assert!(
        !env.tmp.path().join("args").exists(),
        "codex must not start"
    );
}
