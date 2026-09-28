use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Utc};
use serde::Serialize;

use crate::agy::{locate_agy_binary, run_command_captured_interruptibly_with_timeout};
use crate::config::ProfileRegistry;
use crate::keychain::KeychainGuard;
use crate::security::{validate_profile_dir_under, ProcessLock};

pub const USAGE_PROMPT: &str = "/usage";
const DEFAULT_USAGE_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const RESTORE_EMERGENCY_TIMEOUT: Duration = Duration::from_secs(5);
const VIETNAM_UTC_OFFSET_SECONDS: i32 = 7 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageWindow {
    pub remaining_percent: u8,
    pub reset_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeminiUsage {
    pub weekly: UsageWindow,
    pub five_hour: UsageWindow,
}

#[derive(Debug, Clone)]
enum ProfileUsageResult {
    Success { profile: String, usage: GeminiUsage },
    Error { profile: String, error: String },
}

#[derive(Debug, Clone, Serialize)]
struct UsageJsonResult {
    profile: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    five_hour: Option<UsageJsonWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    weekly: Option<UsageJsonWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct UsageJsonOutput {
    schema_version: u32,
    timezone: &'static str,
    results: Vec<UsageJsonResult>,
    cli_exit_code: i32,
}

#[derive(Debug, Clone, Serialize)]
struct UsageJsonWindow {
    remaining_percent: u8,
    reset_at: String,
}

pub fn parse_gemini_usage(raw: &str) -> Result<GeminiUsage, String> {
    let mut weekly = None;
    let mut five_hour = None;

    for line in raw.lines() {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let after_family = match normalized.strip_prefix("Gemini Models ") {
            Some(value) => value,
            None => continue,
        };
        let (label, slot, duplicate_name) = if after_family.starts_with("Weekly Limit Remaining ") {
            ("Weekly Limit Remaining", &mut weekly, "weekly")
        } else if after_family.starts_with("Five Hour Limit Remaining ") {
            ("Five Hour Limit Remaining", &mut five_hour, "five-hour")
        } else {
            continue;
        };

        let remainder = after_family
            .split_once(label)
            .map(|(_, value)| value.trim())
            .ok_or_else(|| format!("Gemini {duplicate_name} quota row is malformed."))?;
        let mut fields = remainder.split_whitespace();
        let percentage = fields
            .next()
            .and_then(|value| value.strip_suffix('%'))
            .ok_or_else(|| "Gemini quota percentage is malformed.".to_string())?
            .parse::<u8>()
            .map_err(|_| "Gemini quota percentage is malformed.".to_string())?;
        if percentage > 100 {
            return Err("Gemini quota percentage is outside 0..=100.".to_string());
        }

        let reset_at = fields
            .next()
            .ok_or_else(|| "Gemini quota reset timestamp is missing.".to_string())?;
        let reset_at = DateTime::parse_from_rfc3339(reset_at)
            .map_err(|_| "Gemini quota reset timestamp is malformed.".to_string())?
            .with_timezone(&Utc);
        let window = UsageWindow {
            remaining_percent: percentage,
            reset_at,
        };

        if slot.replace(window).is_some() {
            return Err(format!("Gemini {duplicate_name} quota row is duplicated."));
        }
    }

    Ok(GeminiUsage {
        weekly: weekly.ok_or_else(|| "Gemini weekly quota row is missing.".to_string())?,
        five_hour: five_hour.ok_or_else(|| "Gemini five-hour quota row is missing.".to_string())?,
    })
}

pub fn format_vietnam_rfc3339(timestamp: DateTime<Utc>) -> String {
    let vietnam = FixedOffset::east_opt(VIETNAM_UTC_OFFSET_SECONDS)
        .expect("Vietnam UTC offset must be valid");
    timestamp.with_timezone(&vietnam).to_rfc3339()
}

pub fn format_vietnam_datetime(timestamp: DateTime<Utc>) -> String {
    let vietnam = FixedOffset::east_opt(VIETNAM_UTC_OFFSET_SECONDS)
        .expect("Vietnam UTC offset must be valid");
    timestamp
        .with_timezone(&vietnam)
        .format("%H:%M:%S %d/%m/%Y (GMT+7)")
        .to_string()
}

pub fn format_usage_text(profile: &str, usage: &GeminiUsage) -> String {
    format!(
        "{}\n  5 giờ: {}% — reset {}\n  Hàng tuần: {}% — reset {}",
        profile,
        usage.five_hour.remaining_percent,
        format_vietnam_datetime(usage.five_hour.reset_at),
        usage.weekly.remaining_percent,
        format_vietnam_datetime(usage.weekly.reset_at),
    )
}

pub fn format_usage_report(results: &[(String, GeminiUsage)]) -> String {
    results
        .iter()
        .map(|(profile, usage)| format_usage_text(profile, usage))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn usage_json_success(profile: &str, usage: &GeminiUsage) -> UsageJsonResult {
    UsageJsonResult {
        profile: profile.to_string(),
        status: "available",
        five_hour: Some(UsageJsonWindow {
            remaining_percent: usage.five_hour.remaining_percent,
            reset_at: format_vietnam_rfc3339(usage.five_hour.reset_at),
        }),
        weekly: Some(UsageJsonWindow {
            remaining_percent: usage.weekly.remaining_percent,
            reset_at: format_vietnam_rfc3339(usage.weekly.reset_at),
        }),
        error: None,
    }
}

fn run_usage_for_profile(
    profile_name: &str,
    registry: &ProfileRegistry,
    agy_binary: &std::path::Path,
) -> Result<(String, GeminiUsage), String> {
    let profile = registry
        .resolve_profile(profile_name)
        .ok_or_else(|| format!("Profile or alias '{profile_name}' not found."))?;
    let canonical = profile.name.clone();
    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let validated_profile_dir = validate_profile_dir_under(&profile.dir, &profiles_root)?;

    let deadline = Instant::now() + DEFAULT_USAGE_TIMEOUT;
    let keychain_guard = KeychainGuard::new_with_backend_until(
        &validated_profile_dir,
        crate::keychain::SystemKeychainBackend,
        deadline,
    )?;

    let mut command = Command::new(agy_binary);
    command.args(["--print", USAGE_PROMPT]);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let output_result = if remaining.is_zero() {
        Err("Usage check timed out.".to_string())
    } else {
        run_command_captured_interruptibly_with_timeout(&mut command, Some(remaining))
    };

    let cleanup_result =
        keychain_guard.sync_and_restore_until(Instant::now() + RESTORE_EMERGENCY_TIMEOUT);
    if let Err(error) = cleanup_result {
        return Err(format!("Keychain restoration failed: {error}"));
    }

    let output = output_result?;
    if output.timed_out {
        return Err("Usage check timed out.".to_string());
    }
    let code = output.status.and_then(|status| status.code());
    if code != Some(0) {
        return Err(format!(
            "AGY usage command failed with exit code {}.",
            code.map_or_else(|| "signal".to_string(), |value| value.to_string())
        ));
    }
    if !output.stderr.is_empty() {
        return Err("AGY usage command returned unexpected stderr.".to_string());
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|_| "AGY usage output is not valid UTF-8.".to_string())?;
    let usage = parse_gemini_usage(&stdout)?;
    Ok((canonical, usage))
}

fn execute_usage(profile_name: Option<&str>) -> Result<Vec<ProfileUsageResult>, String> {
    let config_path = ProfileRegistry::get_config_file_path();
    let registry = ProfileRegistry::load_or_default(&config_path)?;
    let agy_binary = locate_agy_binary()?;
    let lock_path = ProfileRegistry::get_profiles_dir().join("agyp.lock");
    let _lock = ProcessLock::acquire_with_timeout(&lock_path, LOCK_TIMEOUT)?;

    let names = if let Some(profile_name) = profile_name {
        vec![profile_name.to_string()]
    } else {
        let mut names = registry.profiles.keys().cloned().collect::<Vec<_>>();
        names.sort();
        if names.is_empty() {
            return Err("No registered profiles found to check.".to_string());
        }
        names
    };

    Ok(names
        .iter()
        .map(
            |name| match run_usage_for_profile(name, &registry, &agy_binary) {
                Ok((profile, usage)) => ProfileUsageResult::Success { profile, usage },
                Err(error) => ProfileUsageResult::Error {
                    profile: name.clone(),
                    error,
                },
            },
        )
        .collect())
}

pub fn cmd_usage(profile: Option<&str>, json: bool) -> Result<(), i32> {
    let results = match execute_usage(profile) {
        Ok(results) => results,
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "schema_version": 1,
                        "timezone": "Asia/Ho_Chi_Minh",
                        "results": [],
                        "error": error,
                        "cli_exit_code": 1,
                    })
                );
            } else {
                eprintln!("Error: {error}");
            }
            return Err(1);
        }
    };

    let has_errors = results
        .iter()
        .any(|result| matches!(result, ProfileUsageResult::Error { .. }));
    let cli_exit_code = i32::from(has_errors);

    if json {
        let json_results = results
            .iter()
            .map(|result| match result {
                ProfileUsageResult::Success { profile, usage } => {
                    usage_json_success(profile, usage)
                }
                ProfileUsageResult::Error { profile, error } => UsageJsonResult {
                    profile: profile.clone(),
                    status: "error",
                    five_hour: None,
                    weekly: None,
                    error: Some(error.clone()),
                },
            })
            .collect();
        let output = UsageJsonOutput {
            schema_version: 1,
            timezone: "Asia/Ho_Chi_Minh",
            results: json_results,
            cli_exit_code,
        };
        println!(
            "{}",
            serde_json::to_string_pretty(&output).expect("usage JSON serialization must succeed")
        );
    } else {
        for (index, result) in results.iter().enumerate() {
            if index > 0 {
                println!();
            }
            match result {
                ProfileUsageResult::Success { profile, usage } => {
                    println!("{}", format_usage_text(profile, usage));
                }
                ProfileUsageResult::Error { profile, error } => {
                    println!("{profile}\n  Lỗi: {error}");
                }
            }
        }
    }

    if has_errors {
        Err(1)
    } else {
        Ok(())
    }
}
