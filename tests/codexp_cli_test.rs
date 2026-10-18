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

    fn claude(&self) -> PathBuf {
        self.tmp.path().join("claude")
    }

    fn codex(&self) -> PathBuf {
        self.tmp.path().join("dot-codex")
    }

    fn profiles(&self) -> PathBuf {
        self.tmp.path().join("profiles")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_codexp"))
            .args(args)
            .env("CODEXP_PROFILES_DIR", self.profiles())
            .env("CODEXP_CODEX_BIN", self.tmp.path().join("fake-codex"))
            .env("CLAUDE_CONFIG_DIR", self.claude())
            .env("CODEXP_ACTIVE_CODEX_HOME", self.codex())
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

#[test]
fn profiles_share_session_history() {
    let env = Env::new();
    let a = write_export(env.tmp.path(), "a@x.com", "acct-a", 86_400 * 3);
    let b = write_export(env.tmp.path(), "b@x.com", "acct-b", 86_400 * 3);
    assert!(env
        .run(&["import", "c1", "--from", a.to_str().unwrap()])
        .status
        .success());

    // c2 recorded history of its own before history was shared.
    let c2 = env.profiles().join("homes/c2");
    let day = c2.join("sessions/2026/10/18");
    fs::create_dir_all(&day).unwrap();
    fs::write(day.join("rollout-x-t-2.jsonl"), "{}\n").unwrap();
    fs::write(c2.join("history.jsonl"), "{\"session_id\":\"t-2\"}\n").unwrap();
    assert!(env
        .run(&["import", "c2", "--from", b.to_str().unwrap()])
        .status
        .success());

    let shared = env.codex();
    let c1 = env.profiles().join("homes/c1");
    for home in [&c1, &c2] {
        for entry in ["sessions", "history.jsonl"] {
            assert!(fs::symlink_metadata(home.join(entry))
                .unwrap()
                .file_type()
                .is_symlink());
        }
    }
    assert!(c1.join("sessions/2026/10/18/rollout-x-t-2.jsonl").is_file());
    assert_eq!(
        fs::read_to_string(shared.join("history.jsonl")).unwrap(),
        "{\"session_id\":\"t-2\"}\n"
    );

    let out = env.run(&["exec", "c1", "--", "resume", "t-2"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logged_env = fs::read_to_string(env.tmp.path().join("env")).unwrap();
    assert!(logged_env.contains(&format!("CODEX_SQLITE_HOME={}\n", shared.display())));
    let args = fs::read_to_string(env.tmp.path().join("args")).unwrap();
    assert!(
        args.starts_with("resume\n-c\n")
            && args.ends_with("\n--dangerously-bypass-approvals-and-sandbox\nt-2\n"),
        "{args}"
    );
}

#[test]
fn claude_skills_and_mcp_servers_reach_codex() {
    let env = Env::new();
    let claude = env.claude();
    let plugin = env.tmp.path().join("plugin");
    for skill in [
        claude.join("skills/review"),
        claude.join("skills/synced/org/pdf"),
        plugin.join("skills/review"),
    ] {
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "---\nname: x\n---\n").unwrap();
    }
    fs::create_dir_all(claude.join("plugins")).unwrap();
    fs::write(
        claude.join("settings.json"),
        json!({"enabledPlugins": {"tools@market": true, "off@market": false}}).to_string(),
    )
    .unwrap();
    fs::write(
        claude.join("plugins/installed_plugins.json"),
        json!({"plugins": {"tools@market": [{"installPath": plugin}]}}).to_string(),
    )
    .unwrap();
    fs::write(
        plugin.join(".mcp.json"),
        json!({"mcpServers": {"docs": {"type": "http", "url": "https://docs.test/mcp",
            "headers": {"Authorization": "Bearer ${DOCS_TOKEN:-none}"}}}})
        .to_string(),
    )
    .unwrap();
    fs::write(
        claude.join(".claude.json"),
        json!({"mcpServers": {
            "mem": {"type": "stdio", "command": "${CLAUDE_PLUGIN_ROOT}/mem", "args": ["--fast"],
                    "env": {"MEM_KEY": "s3cret"}},
            "old": {"type": "sse", "url": "https://sse.test"}
        }})
        .to_string(),
    )
    .unwrap();

    let export = write_export(env.tmp.path(), "a@x.com", "acct-a", 86_400 * 3);
    assert!(env
        .run(&["import", "c1", "--from", export.to_str().unwrap()])
        .status
        .success());
    let skills = env.profiles().join("homes/c1/skills");
    assert!(fs::metadata(&skills).unwrap().is_dir());
    let mut names: Vec<String> = fs::read_dir(&skills)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["pdf", "review", "tools-review"]);

    let out = env.run(&["exec", "c1", "--", "mcp", "list"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("'old' skipped"));
    let args = fs::read_to_string(env.tmp.path().join("args")).unwrap();
    assert!(
        args.contains(
            "mcp_servers.mem={ command = \"/mem\", args = [\"--fast\"], env_vars = [\"MEM_KEY\"] }"
        ),
        "{args}"
    );
    assert!(
        args.contains("mcp_servers.docs={ url = \"https://docs.test/mcp\", env_http_headers = { \"Authorization\" = \"CODEXP_MCP_SECRET_DOCS_0\" } }"),
        "{args}"
    );
    assert!(!args.contains("s3cret"), "secrets stay out of argv");
    let logged_env = fs::read_to_string(env.tmp.path().join("env")).unwrap();
    assert!(logged_env.contains("MEM_KEY=s3cret"));
    assert!(logged_env.contains("CODEXP_MCP_SECRET_DOCS_0=Bearer none"));

    // A skill removed from Claude disappears from the profile on the next run.
    fs::remove_dir_all(claude.join("skills/review")).unwrap();
    env.run(&["exec", "c1", "--", "mcp", "list"]);
    assert!(fs::symlink_metadata(skills.join("tools-review")).is_err());
    assert!(fs::symlink_metadata(skills.join("review")).is_ok());
}

#[test]
fn active_swaps_plain_codex_login_and_backs_up_once() {
    let env = Env::new();
    let a = write_export(env.tmp.path(), "a@x.com", "acct-a", 86_400 * 3);
    let b = write_export(env.tmp.path(), "b@x.com", "acct-b", 86_400 * 3);
    for (name, file) in [("c1", &a), ("c2", &b)] {
        assert!(env
            .run(&["import", name, "--from", file.to_str().unwrap()])
            .status
            .success());
    }
    let stdout = |args: &[&str]| String::from_utf8_lossy(&env.run(args).stdout).to_string();
    let auth = env.codex().join("auth.json");
    let backup = env.codex().join("auth.json.pre-codexp");

    assert!(stdout(&["active"]).contains("no usable ChatGPT login"));

    // A login codexp did not write is kept aside before the first swap.
    fs::create_dir_all(env.codex()).unwrap();
    fs::copy(&a, &auth).unwrap();
    assert!(stdout(&["active"]).contains("c1 (a@x.com)"));
    assert!(env.run(&["active", "c2"]).status.success());
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        fs::read_to_string(&a).unwrap()
    );
    let active: Value = serde_json::from_str(&fs::read_to_string(&auth).unwrap()).unwrap();
    assert_eq!(active["tokens"]["account_id"], "acct-b");
    assert_eq!(active["tokens"]["refresh_token"], "codexp-refresh-disabled");
    assert_eq!(
        fs::metadata(&auth).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(stdout(&["active"]).contains("c2 (b@x.com)"));

    // Swapping between codexp profiles leaves the original backup alone.
    assert!(env.run(&["active", "c1"]).status.success());
    assert!(stdout(&["active"]).contains("c1 (a@x.com)"));
    assert_eq!(
        fs::read_to_string(&backup).unwrap(),
        fs::read_to_string(&a).unwrap()
    );

    assert_eq!(env.run(&["active", "nope"]).status.code(), Some(1));

    // A running app-server daemon caches the old login, so activation restarts it.
    let log = env.tmp.path().join("daemon-calls");
    fs::write(
        env.tmp.path().join("fake-codex"),
        format!(
            "#!/bin/sh\necho \"$* CODEX_HOME=$CODEX_HOME\" >> '{}'\n\
             [ \"$3\" = version ] && echo '{{\"status\":\"running\"}}'\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    let out = stdout(&["active", "c2"]);
    assert!(
        out.contains("Restarted the Codex app-server daemon"),
        "{out}"
    );
    let calls = fs::read_to_string(&log).unwrap();
    let home = env.codex().display().to_string();
    assert_eq!(
        calls.lines().collect::<Vec<_>>(),
        [
            format!("app-server daemon version CODEX_HOME={home}"),
            format!("app-server daemon restart CODEX_HOME={home}"),
        ]
    );
}

fn sql(db: &Path, statements: &str) -> String {
    let out = Command::new("sqlite3")
        .arg(db)
        .arg(statements)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A `state_5.sqlite` with the columns and tables the migration touches.
fn thread_index(db: &Path, threads: &[(&str, &str, Option<&str>)]) {
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let mut statements = String::from(
        "CREATE TABLE _sqlx_migrations (version INTEGER); INSERT INTO _sqlx_migrations VALUES (61);\
         CREATE TABLE thread_sections (id TEXT PRIMARY KEY); CREATE TABLE projects (id TEXT PRIMARY KEY);\
         CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT, thread_section_id TEXT,\
           section_position INTEGER, section_entered_at_ms INTEGER, project_id TEXT);\
         CREATE TABLE thread_read_receipts (thread_id TEXT PRIMARY KEY);\
         CREATE TABLE thread_dynamic_tools (thread_id TEXT PRIMARY KEY);\
         CREATE TABLE thread_spawn_edges (child_thread_id TEXT PRIMARY KEY);\
         CREATE TABLE thread_attachments (id TEXT PRIMARY KEY);",
    );
    for (id, path, section) in threads {
        let section = section.map_or("NULL".to_string(), |s| format!("'{s}'"));
        statements +=
            &format!("INSERT INTO threads VALUES ('{id}', '{path}', {section}, 1, 1, NULL);");
    }
    sql(db, &statements);
}

#[test]
fn legacy_shared_history_moves_into_plain_codex() {
    let env = Env::new();
    let legacy = env.profiles().join("shared");
    let c1 = env.profiles().join("homes/c1");
    let day = "sessions/2026/10/18";
    fs::create_dir_all(legacy.join(day)).unwrap();
    fs::write(legacy.join(day).join("rollout-x-t-old.jsonl"), "{}\n").unwrap();
    fs::write(legacy.join("history.jsonl"), "old\n").unwrap();
    let old_rollout = c1.join(day).join("rollout-x-t-old.jsonl");
    thread_index(
        &legacy.join("sqlite/state_5.sqlite"),
        &[("t-old", old_rollout.to_str().unwrap(), Some("s-old"))],
    );
    fs::create_dir_all(env.codex()).unwrap();
    fs::write(env.codex().join("history.jsonl"), "plain\n").unwrap();
    let index = env.codex().join("state_5.sqlite");
    thread_index(&index, &[("t-plain", "/elsewhere/sessions/r.jsonl", None)]);

    let a = write_export(env.tmp.path(), "a@x.com", "acct-a", 86_400 * 3);
    let out = env.run(&["import", "c1", "--from", a.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let moved = env.codex().join(day).join("rollout-x-t-old.jsonl");
    assert!(moved.is_file());
    assert!(c1.join(day).join("rollout-x-t-old.jsonl").is_file());
    assert_eq!(
        fs::read_link(c1.join("sessions")).unwrap(),
        env.codex().join("sessions")
    );
    assert_eq!(
        fs::read_to_string(env.codex().join("history.jsonl")).unwrap(),
        "plain\nold\n"
    );
    assert_eq!(
        sql(&index, "SELECT id || '|' || rollout_path || '|' || ifnull(thread_section_id, '-') FROM threads ORDER BY id;"),
        format!("t-old|{}|-\nt-plain|/elsewhere/sessions/r.jsonl|-", moved.display())
    );
    assert!(!legacy.exists());
    assert!(env
        .profiles()
        .join("shared.migrated/sqlite/state_5.sqlite")
        .is_file());

    // Removing a profile repoints the threads it recorded under its own home.
    let own = c1.join("archived_sessions/rollout-y-t-new.jsonl");
    sql(
        &index,
        &format!(
            "INSERT INTO threads VALUES ('t-new', '{}', NULL, NULL, NULL, NULL);",
            own.display()
        ),
    );
    assert!(env.run(&["rm", "c1", "-y"]).status.success());
    assert_eq!(
        sql(
            &index,
            "SELECT rollout_path FROM threads WHERE id = 't-new';"
        ),
        env.codex()
            .join("archived_sessions/rollout-y-t-new.jsonl")
            .display()
            .to_string()
    );
}
