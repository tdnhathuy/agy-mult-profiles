//! Imported Codex (ChatGPT OAuth) credentials.
//!
//! codexp never logs in. Credentials are exported from an upstream owner (e.g. OmniRoute) and
//! imported here. OpenAI refresh tokens rotate on use, so if codexp ever refreshed an imported
//! token the upstream copy would be invalidated. To make that impossible the stored refresh token
//! is replaced with a sentinel and Codex's refresh endpoint is pointed at a dead address at run
//! time (see `exec::build_codex_command`). When the access token expires the profile simply stops
//! working until a fresh export is imported again.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;

use crate::keychain::decode_jwt_payload;

pub const AUTH_FILENAME: &str = "auth.json";
pub const REFRESH_DISABLED_SENTINEL: &str = "codexp-refresh-disabled";

const AUTH_CLAIMS: &str = "https://api.openai.com/auth";
const PROFILE_CLAIMS: &str = "https://api.openai.com/profile";

#[derive(Debug, Clone, PartialEq)]
pub struct AccountSummary {
    pub email: String,
    pub account_id: String,
    pub plan: Option<String>,
    pub access_expires_at: DateTime<Utc>,
}

fn token_str<'a>(tokens: &'a Value, key: &str) -> Result<&'a str, String> {
    tokens
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("missing tokens.{key}"))
}

fn claim_str<'a>(payload: &'a Value, group: &str, key: &str) -> Option<&'a str> {
    payload
        .get(group)
        .and_then(|g| g.get(key))
        .and_then(|v| v.as_str())
}

/// Validates a Codex `auth.json` export and summarizes the account its access token belongs to.
/// Rejects exports whose id_token, access_token and account_id disagree, because Codex would send
/// one account's token with another account's workspace header.
pub fn inspect_auth(auth: &Value) -> Result<AccountSummary, String> {
    if !auth.is_object() {
        return Err("auth file is not a JSON object".to_string());
    }
    if let Some(mode) = auth.get("auth_mode").and_then(|v| v.as_str()) {
        if mode != "chatgpt" {
            return Err(format!(
                "auth_mode is '{mode}'; only ChatGPT OAuth exports are supported"
            ));
        }
    }
    let tokens = auth
        .get("tokens")
        .filter(|t| t.is_object())
        .ok_or("missing tokens object")?;
    let access_token = token_str(tokens, "access_token")?;
    let id_token = token_str(tokens, "id_token")?;
    let account_id = token_str(tokens, "account_id")?;

    let access = decode_jwt_payload(access_token).ok_or("access_token is not a valid JWT")?;
    let id = decode_jwt_payload(id_token).ok_or("id_token is not a valid JWT")?;

    let access_email = claim_str(&access, PROFILE_CLAIMS, "email")
        .or_else(|| access.get("email").and_then(|v| v.as_str()))
        .ok_or("access_token has no email claim")?;
    let id_email = id.get("email").and_then(|v| v.as_str());
    if let Some(id_email) = id_email {
        if !id_email.eq_ignore_ascii_case(access_email) {
            return Err(format!(
                "token mismatch: id_token belongs to {id_email} but access_token belongs to {access_email}; re-export this account"
            ));
        }
    }

    for (label, payload) in [("access_token", &access), ("id_token", &id)] {
        if let Some(claimed) = claim_str(payload, AUTH_CLAIMS, "chatgpt_account_id") {
            if claimed != account_id {
                return Err(format!(
                    "token mismatch: {label} is for account {claimed} but account_id is {account_id}; re-export this account"
                ));
            }
        }
    }

    let exp = access
        .get("exp")
        .and_then(|v| v.as_i64())
        .ok_or("access_token has no exp claim")?;
    let access_expires_at = Utc
        .timestamp_opt(exp, 0)
        .single()
        .ok_or("access_token exp is out of range")?;

    let plan = claim_str(&access, AUTH_CLAIMS, "chatgpt_plan_type")
        .or_else(|| claim_str(&id, AUTH_CLAIMS, "chatgpt_plan_type"))
        .map(str::to_string);

    Ok(AccountSummary {
        email: access_email.to_string(),
        account_id: account_id.to_string(),
        plan,
        access_expires_at,
    })
}

/// Produces the `auth.json` codexp stores: the export with its refresh token replaced by a
/// sentinel and any API key dropped. `last_refresh` is set to `now` so Codex does not attempt its
/// periodic proactive refresh while the imported access token is still valid.
pub fn sanitize_for_import(auth: &Value, now: DateTime<Utc>) -> Result<Value, String> {
    let mut out = auth.clone();
    let obj = out
        .as_object_mut()
        .ok_or("auth file is not a JSON object")?;
    obj.insert("auth_mode".to_string(), Value::from("chatgpt"));
    obj.insert("OPENAI_API_KEY".to_string(), Value::Null);
    obj.insert(
        "last_refresh".to_string(),
        Value::from(now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    let tokens = obj
        .get_mut("tokens")
        .and_then(|t| t.as_object_mut())
        .ok_or("missing tokens object")?;
    tokens.insert(
        "refresh_token".to_string(),
        Value::from(REFRESH_DISABLED_SENTINEL),
    );
    Ok(out)
}

pub fn load_profile_auth(home: &std::path::Path) -> Result<(Value, AccountSummary), String> {
    let path = home.join(AUTH_FILENAME);
    crate::security::reject_symlink(&path).map_err(|e| e.to_string())?;
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let auth: Value = serde_json::from_str(&raw)
        .map_err(|e| format!("invalid JSON in {}: {e}", path.display()))?;
    let summary = inspect_auth(&auth)?;
    Ok((auth, summary))
}
