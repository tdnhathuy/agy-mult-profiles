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
