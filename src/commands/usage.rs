use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, Utc};
use serde::Serialize;

use crate::agy::{locate_agy_binary, run_command_captured_interruptibly_with_timeout};
use crate::config::ProfileRegistry;
use crate::keychain::{activate_profile, apply_profile_home};
use crate::security::{profile_lock_path, validate_profile_dir_under, ProcessLock};

pub const USAGE_PROMPT: &str = "/usage";
pub const DEFAULT_USAGE_TIMEOUT: Duration = Duration::from_secs(60);
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const RESTORE_EMERGENCY_TIMEOUT: Duration = Duration::from_secs(5);
const VIETNAM_UTC_OFFSET_SECONDS: i32 = 7 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageWindow {
    pub remaining_percent: u8,
    pub reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelFamilyUsage {
    pub five_hour: UsageWindow,
    pub five_day: UsageWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GeminiUsage {
    pub gemini: ModelFamilyUsage,
    pub gpt: ModelFamilyUsage,
}

#[derive(Debug, Clone)]
enum ProfileUsageResult {
    Success {
        profile: String,
        email: String,
        usage: GeminiUsage,
    },
    Error {
        profile: String,
        email: String,
        error: String,
    },
}

#[derive(Debug, Clone, Serialize)]
struct UsageJsonResult {
    profile: String,
    email: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    gemini: Option<UsageJsonFamily>,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpt: Option<UsageJsonFamily>,
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
struct UsageJsonFamily {
    five_hour: UsageJsonWindow,
    five_day: UsageJsonWindow,
}

#[derive(Debug, Clone, Serialize)]
struct UsageJsonWindow {
    remaining_percent: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    reset_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resets_in_seconds: Option<i64>,
}

struct FamilySlots {
    label: &'static str,
    five_hour: Option<UsageWindow>,
    five_day: Option<UsageWindow>,
}

pub fn parse_gemini_usage(raw: &str) -> Result<GeminiUsage, String> {
    let mut gemini = FamilySlots {
        label: "Gemini",
        five_hour: None,
        five_day: None,
    };
    let mut gpt = FamilySlots {
        label: "GPT",
        five_hour: None,
        five_day: None,
    };

    for line in raw.lines() {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let (family, after_family) = if let Some(value) = normalized.strip_prefix("Gemini Models ")
        {
            (&mut gemini, value)
        } else if let Some(value) = normalized.strip_prefix("Claude and GPT models ") {
            (&mut gpt, value)
        } else {
            continue;
        };

        let (label, slot, duplicate_name) = if after_family.starts_with("Weekly Limit Remaining ")
            || after_family.starts_with("Five Day Limit Remaining ")
        {
            let label = if after_family.starts_with("Weekly Limit Remaining ") {
                "Weekly Limit Remaining"
            } else {
                "Five Day Limit Remaining"
            };
            (label, &mut family.five_day, "five-day")
        } else if after_family.starts_with("Five Hour Limit Remaining ") {
            (
                "Five Hour Limit Remaining",
                &mut family.five_hour,
                "five-hour",
            )
        } else {
            continue;
        };

        let remainder = after_family
            .split_once(label)
            .map(|(_, value)| value.trim())
            .ok_or_else(|| format!("{} {duplicate_name} quota row is malformed.", family.label))?;
        let mut fields = remainder.split_whitespace();
        let percentage_field = fields
            .next()
            .ok_or_else(|| format!("{} quota percentage is malformed.", family.label))?;
        let percentage = if percentage_field.eq_ignore_ascii_case("disabled")
            || percentage_field.eq_ignore_ascii_case("unavailable")
        {
            0
        } else {
            percentage_field
                .strip_suffix('%')
                .ok_or_else(|| {
                    format!(
                        "{} quota percentage is malformed: {percentage_field}.",
                        family.label
                    )
                })?
                .parse::<u8>()
                .map_err(|_| format!("{} quota percentage is malformed.", family.label))?
        };
        if percentage > 100 {
            return Err(format!(
                "{} quota percentage is outside 0..=100.",
                family.label
            ));
        }

        let reset_at = match fields.next() {
            Some(value) => Some(
                DateTime::parse_from_rfc3339(value)
                    .map_err(|_| format!("{} quota reset timestamp is malformed.", family.label))?
                    .with_timezone(&Utc),
            ),
            None if percentage_field.eq_ignore_ascii_case("disabled")
                || percentage_field.eq_ignore_ascii_case("unavailable") =>
            {
                None
            }
            None => {
                return Err(format!(
                    "{} quota reset timestamp is missing.",
                    family.label
                ));
            }
        };
        let window = UsageWindow {
            remaining_percent: percentage,
            reset_at,
        };

        if slot.replace(window).is_some() {
            return Err(format!(
                "{} {duplicate_name} quota row is duplicated.",
                family.label
            ));
        }
    }

    Ok(GeminiUsage {
        gemini: finish_family(gemini)?,
        gpt: finish_family(gpt)?,
    })
}

fn finish_family(family: FamilySlots) -> Result<ModelFamilyUsage, String> {
    Ok(ModelFamilyUsage {
        five_hour: family
            .five_hour
            .ok_or_else(|| format!("{} five-hour quota row is missing.", family.label))?,
        five_day: family
            .five_day
            .ok_or_else(|| format!("{} five-day quota row is missing.", family.label))?,
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

pub fn format_remaining_until(reset_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let mut seconds = (reset_at - now).num_seconds();
    if seconds <= 0 {
        return "đã reset".to_string();
    }
    if seconds % 60 != 0 {
        seconds += 60 - (seconds % 60);
    }
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    match (days, hours, minutes) {
        (0, 0, 0) => format!("còn {seconds}s"),
        (0, 0, minutes) => format!("còn {minutes} phút"),
        (0, hours, 0) => format!("còn {hours} giờ"),
        (0, hours, minutes) => format!("còn {hours} giờ {minutes} phút"),
        (days, 0, 0) => format!("còn {days} ngày"),
        (days, hours, 0) => format!("còn {days} ngày {hours} giờ"),
        (days, hours, minutes) => format!("còn {days} ngày {hours} giờ {minutes} phút"),
    }
}

pub(crate) struct Style {
    pub(crate) color: bool,
}

impl Style {
    pub(crate) fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{code}m{text}\x1b[0m")
        } else {
            text.to_string()
        }
    }

    fn percent(&self, percent: u8, disabled: bool) -> String {
        let text = format!("{percent:>3}%");
        let code = if disabled {
            "1;90"
        } else if percent >= 70 {
            "1;32"
        } else if percent >= 30 {
            "1;33"
        } else {
            "1;31"
        };
        self.paint(code, &text)
    }

    fn bar(&self, percent: u8) -> String {
        let filled = (usize::from(percent) * 10) / 100;
        let empty = 10 - filled;
        let blocks = format!("{}{}", "█".repeat(filled), "░".repeat(empty));
        if !self.color {
            return blocks;
        }
        let code = if percent >= 70 {
            "32"
        } else if percent >= 30 {
            "33"
        } else {
            "31"
        };
        format!(
            "\x1b[{code}m{}\x1b[2;90m{}\x1b[0m",
            "█".repeat(filled),
            "░".repeat(empty)
        )
    }
}

fn pad_chars(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        value.to_string()
    } else {
        format!("{value}{}", " ".repeat(width - len))
    }
}

pub(crate) fn format_window_line(
    style: &Style,
    label: &str,
    window: &UsageWindow,
    now: DateTime<Utc>,
) -> String {
    let label = style.paint("2", &pad_chars(label, 6));
    match window.reset_at {
        Some(reset_at) => format!(
            "    {label}  {}  {}  {}",
            style.percent(window.remaining_percent, false),
            style.bar(window.remaining_percent),
            style.paint("2", &format_remaining_until(reset_at, now)),
        ),
        None => format!(
            "    {label}  {}  {}",
            style.percent(window.remaining_percent, true),
            style.paint("31", "đã tắt"),
        ),
    }
}

fn format_family_block(
    style: &Style,
    title: &str,
    family: &ModelFamilyUsage,
    now: DateTime<Utc>,
) -> String {
    let title = if title == "Gemini" {
        style.paint("1;34", title)
    } else {
        style.paint("1;35", title)
    };
    format!(
        "  {title}\n{}\n{}",
        format_window_line(style, "5 giờ", &family.five_hour, now),
        format_window_line(style, "5 ngày", &family.five_day, now),
    )
}

fn render_usage_text(
    profile: &str,
    email: &str,
    usage: &GeminiUsage,
    now: DateTime<Utc>,
    color: bool,
    show_gpt: bool,
) -> String {
    let style = Style { color };
    let mut rendered = format!(
        "{}  {}\n{}",
        style.paint("1;36", profile),
        style.paint("2", email),
        format_family_block(&style, "Gemini", &usage.gemini, now),
    );
    if show_gpt {
        rendered.push('\n');
        rendered.push_str(&format_family_block(&style, "GPT", &usage.gpt, now));
    }
    rendered
}

pub fn format_usage_text(profile: &str, email: &str, usage: &GeminiUsage) -> String {
    format_usage_text_at(profile, email, usage, Utc::now())
}

pub fn format_usage_text_at(
    profile: &str,
    email: &str,
    usage: &GeminiUsage,
    now: DateTime<Utc>,
) -> String {
    render_usage_text(profile, email, usage, now, false, true)
}

pub fn format_usage_text_colored_at(
    profile: &str,
    email: &str,
    usage: &GeminiUsage,
    now: DateTime<Utc>,
    show_gpt: bool,
) -> String {
    render_usage_text(profile, email, usage, now, true, show_gpt)
}

pub fn usage_color_enabled() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stdout())
}

pub fn format_usage_report(results: &[(String, String, GeminiUsage)]) -> String {
    results
        .iter()
        .map(|(profile, email, usage)| format_usage_text(profile, email, usage))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn usage_json_window(window: &UsageWindow, now: DateTime<Utc>) -> UsageJsonWindow {
    UsageJsonWindow {
        remaining_percent: window.remaining_percent,
        reset_at: window.reset_at.map(format_vietnam_rfc3339),
        resets_in_seconds: window
            .reset_at
            .map(|reset_at| (reset_at - now).num_seconds().max(0)),
    }
}

fn usage_json_family(family: &ModelFamilyUsage, now: DateTime<Utc>) -> UsageJsonFamily {
    UsageJsonFamily {
        five_hour: usage_json_window(&family.five_hour, now),
        five_day: usage_json_window(&family.five_day, now),
    }
}

fn usage_json_success(
    profile: &str,
    email: &str,
    usage: &GeminiUsage,
    now: DateTime<Utc>,
    show_gpt: bool,
) -> UsageJsonResult {
    UsageJsonResult {
        profile: profile.to_string(),
        email: email.to_string(),
        status: "available",
        gemini: Some(usage_json_family(&usage.gemini, now)),
        gpt: show_gpt.then(|| usage_json_family(&usage.gpt, now)),
        error: None,
    }
}

/// AGY's `/usage` run for one profile. `--gemini_dir` is required: without it AGY 1.4+ looks
/// for its login in the shared `~/.gemini` and blocks on a browser sign-in instead.
pub fn usage_command(agy_binary: &std::path::Path, profile_dir: &std::path::Path) -> Command {
    let mut command = Command::new(agy_binary);
    command.arg(format!("--gemini_dir={}", profile_dir.display()));
    command.args(["--print", USAGE_PROMPT]);
    command
}

fn run_usage_for_profile(
    profile_name: &str,
    registry: &ProfileRegistry,
    agy_binary: &std::path::Path,
) -> Result<(String, String, GeminiUsage), String> {
    let profile = registry
        .resolve_profile(profile_name)
        .ok_or_else(|| format!("Profile or alias '{profile_name}' not found."))?;
    let canonical = profile.name.clone();
    let email = profile.email.clone();
    let profiles_root = ProfileRegistry::get_profiles_dir().join("profiles");
    let validated_profile_dir = validate_profile_dir_under(&profile.dir, &profiles_root)?;
    // Acquiring the profile lock re-arms the signal handlers; honor an earlier interrupt first.
    if ProcessLock::interrupted() {
        return Err("Interrupted.".to_string());
    }
    let lock_path = profile_lock_path(&ProfileRegistry::get_profiles_dir(), &canonical);
    let lock = ProcessLock::acquire_with_timeout(&lock_path, LOCK_TIMEOUT)?;

    let deadline = Instant::now() + DEFAULT_USAGE_TIMEOUT;
    let (keychain_guard, profile_home) = activate_profile(
        &validated_profile_dir,
        crate::keychain::SystemKeychainBackend,
        deadline,
        lock,
    )?;

    let mut command = usage_command(agy_binary, &validated_profile_dir);
    apply_profile_home(&mut command, profile_home.as_deref());
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
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.to_ascii_lowercase().contains("authentication") {
            return Err(
                "AGY authentication required. Run `agyp login <profile>` and retry.".to_string(),
            );
        }
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
    Ok((canonical, email, usage))
}

fn execute_usage(profile_name: Option<&str>) -> Result<Vec<ProfileUsageResult>, String> {
    let config_path = ProfileRegistry::get_config_file_path();
    let registry = ProfileRegistry::load_or_default(&config_path)?;
    let agy_binary = locate_agy_binary()?;

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
                Ok((profile, email, usage)) => ProfileUsageResult::Success {
                    profile,
                    email,
                    usage,
                },
                Err(error) => ProfileUsageResult::Error {
                    profile: name.clone(),
                    email: registry
                        .resolve_profile(name)
                        .map(|profile| profile.email.clone())
                        .unwrap_or_else(|| "unknown".to_string()),
                    error,
                },
            },
        )
        .collect())
}

pub fn cmd_usage(profile: Option<&str>, json: bool, show_gpt: bool) -> Result<(), i32> {
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
                ProfileUsageResult::Success {
                    profile,
                    email,
                    usage,
                } => usage_json_success(profile, email, usage, Utc::now(), show_gpt),
                ProfileUsageResult::Error {
                    profile,
                    email,
                    error,
                } => UsageJsonResult {
                    profile: profile.clone(),
                    email: email.clone(),
                    status: "error",
                    gemini: None,
                    gpt: None,
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
                ProfileUsageResult::Success {
                    profile,
                    email,
                    usage,
                } => {
                    let rendered = if usage_color_enabled() {
                        format_usage_text_colored_at(profile, email, usage, Utc::now(), show_gpt)
                    } else {
                        render_usage_text(profile, email, usage, Utc::now(), false, show_gpt)
                    };
                    println!("{rendered}");
                }
                ProfileUsageResult::Error {
                    profile,
                    email,
                    error,
                } => {
                    if usage_color_enabled() {
                        println!(
                            "\x1b[1;36m{profile}\x1b[0m  \x1b[2m{email}\x1b[0m\n    \x1b[31mLỗi: {error}\x1b[0m"
                        );
                    } else {
                        println!("{profile}  {email}\n    Lỗi: {error}");
                    }
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
