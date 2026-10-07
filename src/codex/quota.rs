//! `codexp q`: ChatGPT Codex rate-limit windows per profile.
//!
//! Reads the same usage endpoint Codex's `/status` uses, with the profile's imported access token.
//! It is a read-only request: no refresh, so the upstream (OmniRoute) token is untouched.

use std::io::Write;
use std::process::{Command, Stdio};
use std::thread;

use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::Value;

use super::auth::load_profile_auth;
use super::home::registry_path;
use crate::commands::usage::{
    format_remaining_until, format_vietnam_rfc3339, format_window_line, usage_color_enabled, Style,
    UsageWindow,
};
use crate::config::{ProfileInfo, ProfileRegistry};

pub const DEFAULT_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const REQUEST_TIMEOUT_SECS: &str = "20";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexWindow {
    pub used_percent: f64,
    pub window_seconds: Option<i64>,
    pub reset_at: Option<DateTime<Utc>>,
}

impl CodexWindow {
    pub fn remaining_percent(&self) -> u8 {
        (100.0 - self.used_percent).clamp(0.0, 100.0).round() as u8
    }

    /// "5 giờ", "7 ngày", ... from the window length; Codex reports 5h and weekly windows.
    pub fn label(&self) -> String {
        match self.window_seconds {
            Some(s) if s > 0 && s % 86_400 == 0 => format!("{} ngày", s / 86_400),
            Some(s) if s > 0 && s % 3_600 == 0 => format!("{} giờ", s / 3_600),
            Some(s) if s > 0 => format!("{} phút", s / 60),
            _ => "?".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodexUsage {
    pub plan: Option<String>,
    pub allowed: bool,
    pub limit_reached: bool,
    pub primary: Option<CodexWindow>,
    pub secondary: Option<CodexWindow>,
}

fn parse_window(v: Option<&Value>, now: DateTime<Utc>) -> Option<CodexWindow> {
    let v = v.filter(|v| v.is_object())?;
    let used_percent = v.get("used_percent").and_then(Value::as_f64)?;
    let reset_at = v
        .get("reset_at")
        .and_then(Value::as_i64)
        .and_then(|t| Utc.timestamp_opt(t, 0).single())
        .or_else(|| {
            v.get("reset_after_seconds")
                .and_then(Value::as_i64)
                .map(|s| now + chrono::Duration::seconds(s))
        });
    Some(CodexWindow {
        used_percent,
        window_seconds: v.get("limit_window_seconds").and_then(Value::as_i64),
        reset_at,
    })
}

/// Parses the usage endpoint body (`plan_type` + `rate_limit.{primary,secondary}_window`).
pub fn parse_usage(body: &Value, now: DateTime<Utc>) -> Result<CodexUsage, String> {
    let rate_limit = body
        .get("rate_limit")
        .filter(|v| v.is_object())
        .ok_or("usage response has no rate_limit")?;
    Ok(CodexUsage {
        plan: body
            .get("plan_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        allowed: rate_limit
            .get("allowed")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        limit_reached: rate_limit
            .get("limit_reached")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        primary: parse_window(rate_limit.get("primary_window"), now),
        secondary: parse_window(rate_limit.get("secondary_window"), now),
    })
}

fn usage_url() -> String {
    std::env::var("CODEXP_USAGE_URL").unwrap_or_else(|_| DEFAULT_USAGE_URL.to_string())
}

/// GETs the usage endpoint with curl. Credentials go through curl's stdin config, never argv,
/// so they do not show up in `ps`.
fn fetch_usage(access_token: &str, account_id: &str) -> Result<Value, String> {
    let mut child = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            REQUEST_TIMEOUT_SECS,
            "-K",
            "-",
            "-w",
            "\n%{http_code}",
        ])
        .arg(usage_url())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("không chạy được curl: {e}"))?;
    {
        let mut stdin = child.stdin.take().ok_or("curl stdin unavailable")?;
        let config = format!(
            "header = \"Authorization: Bearer {access_token}\"\nheader = \"ChatGPT-Account-Id: {account_id}\"\nheader = \"Accept: application/json\"\nheader = \"User-Agent: codexp/{}\"\n",
            env!("CARGO_PKG_VERSION")
        );
        stdin
            .write_all(config.as_bytes())
            .map_err(|e| format!("curl stdin: {e}"))?;
    }
    let out = child.wait_with_output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("request failed: {}", err.trim()));
    }
    let raw = String::from_utf8_lossy(&out.stdout);
    let (body, code) = raw.rsplit_once('\n').unwrap_or(("", raw.as_ref()));
    match code.trim() {
        "200" => serde_json::from_str(body).map_err(|e| format!("invalid usage JSON: {e}")),
        "401" => Err("token bị từ chối (401): export lại từ OmniRoute rồi import".to_string()),
        other => Err(format!("HTTP {other}")),
    }
}

#[derive(Debug, Clone)]
enum QuotaResult {
    Ok {
        profile: String,
        email: String,
        usage: CodexUsage,
    },
    Err {
        profile: String,
        email: String,
        error: String,
    },
}

fn quota_for(profile: &ProfileInfo) -> QuotaResult {
    let fail = |email: String, error: String| QuotaResult::Err {
        profile: profile.name.clone(),
        email,
        error,
    };
    let (auth, summary) = match load_profile_auth(&profile.dir) {
        Ok(v) => v,
        Err(e) => return fail(profile.email.clone(), e),
    };
    if summary.access_expires_at <= Utc::now() {
        return fail(
            summary.email,
            "token hết hạn: export lại từ OmniRoute rồi import".to_string(),
        );
    }
    let token = auth["tokens"]["access_token"].as_str().unwrap_or_default();
    match fetch_usage(token, &summary.account_id).and_then(|b| parse_usage(&b, Utc::now())) {
        Ok(usage) => QuotaResult::Ok {
            profile: profile.name.clone(),
            email: summary.email,
            usage,
        },
        Err(e) => fail(summary.email, e),
    }
}

fn selected_profiles(name: Option<&str>) -> Result<Vec<ProfileInfo>, String> {
    let reg = ProfileRegistry::load_or_default(&registry_path())?;
    if let Some(name) = name {
        let p = reg
            .resolve_profile(name)
            .cloned()
            .ok_or_else(|| format!("profile or alias '{name}' not found"))?;
        return Ok(vec![p]);
    }
    let mut all: Vec<ProfileInfo> = reg.profiles.into_values().collect();
    all.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(all)
}

#[derive(Serialize)]
struct JsonWindow {
    label: String,
    used_percent: f64,
    remaining_percent: u8,
    window_seconds: Option<i64>,
    reset_at: Option<String>,
    reset_in: Option<String>,
}

fn json_window(w: &Option<CodexWindow>, now: DateTime<Utc>) -> Option<JsonWindow> {
    w.as_ref().map(|w| JsonWindow {
        label: w.label(),
        used_percent: w.used_percent,
        remaining_percent: w.remaining_percent(),
        window_seconds: w.window_seconds,
        reset_at: w.reset_at.map(format_vietnam_rfc3339),
        reset_in: w.reset_at.map(|r| format_remaining_until(r, now)),
    })
}

fn render_text(result: &QuotaResult, now: DateTime<Utc>, color: bool) -> String {
    let style = Style { color };
    match result {
        QuotaResult::Ok {
            profile,
            email,
            usage,
        } => {
            let mut head = format!(
                "{}  {}  {}",
                style.paint("1;36", profile),
                style.paint("2", email),
                style.paint("1;35", usage.plan.as_deref().unwrap_or("?")),
            );
            if usage.limit_reached || !usage.allowed {
                head.push_str(&format!("  {}", style.paint("1;31", "ĐÃ CHẠM GIỚI HẠN")));
            }
            let mut lines = vec![head];
            for w in [&usage.primary, &usage.secondary].into_iter().flatten() {
                let window = UsageWindow {
                    remaining_percent: w.remaining_percent(),
                    reset_at: w.reset_at,
                };
                lines.push(format_window_line(&style, &w.label(), &window, now));
            }
            if usage.primary.is_none() && usage.secondary.is_none() {
                lines.push(format!(
                    "    {}",
                    style.paint("2", "không có giới hạn theo cửa sổ")
                ));
            }
            lines.join("\n")
        }
        QuotaResult::Err {
            profile,
            email,
            error,
        } => format!(
            "{}  {}\n    {}",
            style.paint("1;36", profile),
            style.paint("2", email),
            style.paint("31", &format!("Lỗi: {error}"))
        ),
    }
}

pub fn cmd_quota(profile: Option<&str>, json: bool) -> Result<(), i32> {
    let profiles = match selected_profiles(profile) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {e}");
            return Err(1);
        }
    };
    let handles: Vec<_> = profiles
        .into_iter()
        .map(|p| thread::spawn(move || quota_for(&p)))
        .collect();
    let results: Vec<QuotaResult> = handles
        .into_iter()
        .map(|h| h.join().expect("quota worker panicked"))
        .collect();
    let now = Utc::now();
    let has_errors = results.iter().any(|r| matches!(r, QuotaResult::Err { .. }));

    if json {
        let rows: Vec<Value> = results
            .iter()
            .map(|r| match r {
                QuotaResult::Ok {
                    profile,
                    email,
                    usage,
                } => serde_json::json!({
                    "profile": profile,
                    "email": email,
                    "status": if usage.limit_reached || !usage.allowed { "limit_reached" } else { "ok" },
                    "plan": usage.plan,
                    "primary": json_window(&usage.primary, now),
                    "secondary": json_window(&usage.secondary, now),
                }),
                QuotaResult::Err {
                    profile,
                    email,
                    error,
                } => serde_json::json!({
                    "profile": profile,
                    "email": email,
                    "status": "error",
                    "error": error,
                }),
            })
            .collect();
        let out = serde_json::json!({
            "schema_version": 1,
            "timezone": "Asia/Ho_Chi_Minh",
            "checked_at": format_vietnam_rfc3339(now),
            "results": rows,
            "cli_exit_code": i32::from(has_errors),
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        let color = usage_color_enabled();
        let blocks: Vec<String> = results.iter().map(|r| render_text(r, now, color)).collect();
        println!("{}", blocks.join("\n\n"));
    }
    if has_errors {
        Err(1)
    } else {
        Ok(())
    }
}
