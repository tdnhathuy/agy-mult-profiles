use agyp::codex::auth::{inspect_auth, sanitize_for_import, REFRESH_DISABLED_SENTINEL};
use agyp::codex::exec::{classify_exec, parse_exec_events};
use agyp::commands::task::TaskFailureKind;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use serde_json::{json, Value};

fn jwt(payload: Value) -> String {
    format!(
        "{}.{}.sig",
        URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    )
}

fn export(
    id_email: &str,
    access_email: &str,
    token_account: &str,
    file_account: &str,
    exp: i64,
) -> Value {
    json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": null,
        "tokens": {
            "id_token": jwt(json!({
                "email": id_email,
                "exp": 1,
                "https://api.openai.com/auth": {"chatgpt_account_id": token_account, "chatgpt_plan_type": "plus"}
            })),
            "access_token": jwt(json!({
                "exp": exp,
                "https://api.openai.com/profile": {"email": access_email},
                "https://api.openai.com/auth": {"chatgpt_account_id": token_account}
            })),
            "refresh_token": "real-upstream-refresh-token",
            "account_id": file_account
        },
        "last_refresh": "2026-10-06T07:10:00.000Z"
    })
}

fn future() -> i64 {
    Utc::now().timestamp() + 7 * 86_400
}

#[test]
fn accepts_consistent_export_even_with_expired_id_token() {
    let s = inspect_auth(&export("a@x.com", "a@x.com", "acct-1", "acct-1", future())).unwrap();
    assert_eq!(s.email, "a@x.com");
    assert_eq!(s.account_id, "acct-1");
    assert_eq!(s.plan.as_deref(), Some("plus"));
}

#[test]
fn rejects_email_mismatch_between_id_and_access_token() {
    let err =
        inspect_auth(&export("a@x.com", "b@x.com", "acct-1", "acct-1", future())).unwrap_err();
    assert!(err.contains("mismatch"), "{err}");
}

#[test]
fn rejects_account_id_mismatch() {
    let err =
        inspect_auth(&export("a@x.com", "a@x.com", "acct-1", "acct-2", future())).unwrap_err();
    assert!(err.contains("account"), "{err}");
}

#[test]
fn rejects_api_key_auth_mode() {
    let mut e = export("a@x.com", "a@x.com", "acct-1", "acct-1", future());
    e["auth_mode"] = json!("apikey");
    assert!(inspect_auth(&e).is_err());
}

#[test]
fn sanitize_drops_upstream_refresh_token() {
    let now = Utc::now();
    let out = sanitize_for_import(
        &export("a@x.com", "a@x.com", "acct-1", "acct-1", future()),
        now,
    )
    .unwrap();
    assert_eq!(out["tokens"]["refresh_token"], REFRESH_DISABLED_SENTINEL);
    assert!(!out.to_string().contains("real-upstream-refresh-token"));
    assert_eq!(out["OPENAI_API_KEY"], Value::Null);
    let refreshed: chrono::DateTime<Utc> = out["last_refresh"].as_str().unwrap().parse().unwrap();
    assert!((refreshed - now).num_seconds().abs() <= 1);
    inspect_auth(&out).unwrap();
}

const OK_STREAM: &str = r#"{"type":"thread.started","thread_id":"t-123"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"i0","type":"reasoning","text":"thinking"}}
{"type":"item.completed","item":{"id":"i1","type":"agent_message","text":"first"}}
{"type":"item.completed","item":{"id":"i2","type":"agent_message","text":"Done: rate limit handling added"}}
{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":5}}
"#;

#[test]
fn parses_thread_final_message_and_usage() {
    let s = parse_exec_events(OK_STREAM);
    assert_eq!(s.thread_id.as_deref(), Some("t-123"));
    assert_eq!(
        s.final_message.as_deref(),
        Some("Done: rate limit handling added")
    );
    assert_eq!(s.usage.unwrap()["output_tokens"], 5);
    assert!(s.turn_completed);
}

#[test]
fn agent_prose_about_rate_limits_is_still_success() {
    let s = parse_exec_events(OK_STREAM);
    assert_eq!(classify_exec(false, Some(0), &s, ""), Ok(()));
}

#[test]
fn classifies_usage_limit_as_quota() {
    let s = parse_exec_events(
        "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n{\"type\":\"turn.failed\",\"error\":{\"message\":\"You've hit your usage limit. Try again later.\"}}\n",
    );
    assert_eq!(
        classify_exec(false, Some(1), &s, ""),
        Err(TaskFailureKind::QuotaExhausted)
    );
}

#[test]
fn classifies_refresh_failure_as_unauthenticated() {
    let s = parse_exec_events(
        "{\"type\":\"error\",\"message\":\"Your access token could not be refreshed. Please log out and sign in again.\"}\n",
    );
    assert_eq!(
        classify_exec(false, Some(1), &s, ""),
        Err(TaskFailureKind::Unauthenticated)
    );
}

#[test]
fn classifies_timeout_empty_and_generic_failures() {
    let empty = parse_exec_events("{\"type\":\"turn.completed\"}\n");
    assert_eq!(
        classify_exec(true, None, &empty, ""),
        Err(TaskFailureKind::Timeout)
    );
    assert_eq!(
        classify_exec(false, Some(0), &empty, ""),
        Err(TaskFailureKind::EmptyOutput)
    );
    assert_eq!(
        classify_exec(false, Some(3), &empty, "boom"),
        Err(TaskFailureKind::AgentFailed)
    );
}

#[test]
fn parses_usage_windows_and_labels() {
    use agyp::codex::quota::parse_usage;
    let now = Utc::now();
    let body = json!({
        "plan_type": "plus",
        "rate_limit": {
            "allowed": true,
            "limit_reached": false,
            "primary_window": {"used_percent": 9, "limit_window_seconds": 18000, "reset_after_seconds": 3600},
            "secondary_window": {"used_percent": 56.4, "limit_window_seconds": 604800, "reset_at": now.timestamp() + 86_400}
        }
    });
    let u = parse_usage(&body, now).unwrap();
    assert_eq!(u.plan.as_deref(), Some("plus"));
    let p = u.primary.unwrap();
    assert_eq!((p.label().as_str(), p.remaining_percent()), ("5 giờ", 91));
    assert_eq!(p.reset_at.unwrap(), now + chrono::Duration::seconds(3600));
    let s = u.secondary.unwrap();
    assert_eq!((s.label().as_str(), s.remaining_percent()), ("7 ngày", 44));
}

#[test]
fn parses_free_plan_single_window_and_limit_reached() {
    use agyp::codex::quota::parse_usage;
    let body = json!({
        "plan_type": "free",
        "rate_limit": {
            "allowed": false,
            "limit_reached": true,
            "primary_window": {"used_percent": 100, "limit_window_seconds": 2592000, "reset_after_seconds": 10},
            "secondary_window": null
        }
    });
    let u = parse_usage(&body, Utc::now()).unwrap();
    assert!(u.limit_reached && !u.allowed);
    assert_eq!(u.primary.unwrap().label(), "30 ngày");
    assert!(u.secondary.is_none());
    assert!(parse_usage(&json!({"detail": "nope"}), Utc::now()).is_err());
}

#[test]
fn exec_places_overrides_and_bypass_where_codex_reads_them() {
    use agyp::codex::commands::exec_args;
    let v = |args: &[&str]| args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let flag = "--dangerously-bypass-approvals-and-sandbox";
    let bare = |args: &[&str]| exec_args(&v(args), &[]);

    assert_eq!(bare(&[]), v(&[flag]));
    assert_eq!(bare(&["fix the bug"]), v(&[flag, "fix the bug"]));
    assert_eq!(bare(&["-m", "gpt-5.5"]), v(&[flag, "-m", "gpt-5.5"]));
    assert_eq!(
        bare(&["exec", "resume", "--last"]),
        v(&["exec", flag, "resume", "--last"])
    );
    assert_eq!(bare(&["resume", "--last"]), v(&["resume", flag, "--last"]));
    // Non-agent subcommands and an explicit policy get no bypass.
    assert_eq!(bare(&["mcp", "list"]), v(&["mcp", "list"]));
    assert_eq!(bare(&["-s", "read-only"]), v(&["-s", "read-only"]));
    assert_eq!(
        bare(&["exec", "--ask-for-approval=on-request", "hi"]),
        v(&["exec", "--ask-for-approval=on-request", "hi"])
    );

    // Agent subcommands read overrides only after their own name.
    let config = v(&["-c", "k=1"]);
    assert_eq!(
        exec_args(&v(&["exec", "hi"]), &config),
        v(&["exec", "-c", "k=1", flag, "hi"])
    );
    assert_eq!(
        exec_args(&v(&["mcp", "list"]), &config),
        v(&["-c", "k=1", "mcp", "list"])
    );
    assert_eq!(exec_args(&[], &config), v(&["-c", "k=1", flag]));
}

#[test]
fn plain_codex_sessions_skips_codexp_runs_apps_and_own_ancestors() {
    let ps = "\
      1     0 /sbin/launchd
    100     1 -/opt/homebrew/bin/fish -l
    200   100 /Users/u/.local/bin/codex
    201   100 codex resume --last
    300   100 codexp exec p2
    301   300 /Users/u/.local/bin/codex -c model_provider=\"openai\"
    400     1 /Users/u/.codex/packages/app-server-daemon/releases/0.163.0/bin/codex app-server --listen unix:// --managed-daemon
    500     1 /Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex -c x=1 exec-server
    600     1 /Users/u/.vscode/extensions/openai.chatgpt-1/bin/macos-aarch64/codex app-server
    700   100 /Users/u/.local/bin/codex
    701   700 /bin/zsh -lc codexp active p1
    702   701 codexp active p1
    800   100 /Users/u/.codex/packages/standalone/bin/codex-code-mode-host
";
    assert_eq!(
        agyp::codex::exec::plain_codex_sessions(ps, 702),
        vec![200, 201]
    );
}
