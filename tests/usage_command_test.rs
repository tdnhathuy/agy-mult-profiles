#[path = "../src/agy.rs"]
mod agy;
#[path = "../src/commands/mod.rs"]
#[allow(dead_code)]
mod commands;
#[path = "../src/config.rs"]
mod config;
#[path = "../src/keychain.rs"]
mod keychain;
#[path = "../src/security.rs"]
mod security;

use chrono::{TimeZone, Utc};
use std::time::Duration;

#[test]
fn usage_transaction_timeout_allows_sixty_seconds_per_profile() {
    assert_eq!(
        commands::usage::DEFAULT_USAGE_TIMEOUT,
        Duration::from_secs(60)
    );
}

#[test]
fn parses_gemini_and_gpt_usage_rows() {
    let raw = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    );

    let usage = commands::usage::parse_gemini_usage(raw).unwrap();
    assert_eq!(usage.gemini.five_day.remaining_percent, 42);
    assert_eq!(usage.gemini.five_hour.remaining_percent, 79);
    assert_eq!(usage.gpt.five_day.remaining_percent, 100);
    assert_eq!(usage.gpt.five_hour.remaining_percent, 88);
    assert_eq!(
        usage.gemini.five_day.reset_at.unwrap().to_rfc3339(),
        "2026-10-01T17:26:35+00:00"
    );
    assert_eq!(
        usage.gemini.five_hour.reset_at.unwrap().to_rfc3339(),
        "2026-09-28T06:58:39+00:00"
    );
    assert_eq!(
        usage.gpt.five_hour.reset_at.unwrap().to_rfc3339(),
        "2026-09-28T07:27:35+00:00"
    );
}

#[test]
fn vietnam_datetime_uses_dd_mm_yyyy_and_utc_plus_seven() {
    let dt = Utc.with_ymd_and_hms(2026, 10, 1, 17, 26, 35).unwrap();
    assert_eq!(
        commands::usage::format_vietnam_datetime(dt),
        "00:26:35 02/10/2026 (GMT+7)"
    );
}

#[test]
fn ignores_unknown_gemini_rows_and_accepts_space_alignment() {
    let raw = concat!(
        "Gemini Models  Experimental Weekly Limit Remaining  1%  2026-10-09T17:26:35Z\n",
        "Gemini Models  Experimental Burst Limit Remaining  unavailable  later\n",
        "Gemini Models          Weekly Limit Remaining     42%   2026-10-01T17:26:35Z extra\n",
        "Gemini Models          Five Hour Limit Remaining  79%   2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    );

    let usage = commands::usage::parse_gemini_usage(raw).unwrap();
    assert_eq!(usage.gemini.five_day.remaining_percent, 42);
    assert_eq!(usage.gemini.five_hour.remaining_percent, 79);
}

#[test]
fn vietnam_rfc3339_is_machine_readable_with_plus_seven_offset() {
    let dt = Utc.with_ymd_and_hms(2026, 10, 1, 17, 26, 35).unwrap();
    assert_eq!(
        commands::usage::format_vietnam_rfc3339(dt),
        "2026-10-02T00:26:35+07:00"
    );
}

fn sample_usage_raw() -> String {
    concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tFive Day Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    )
    .to_string()
}

#[test]
fn quick_text_includes_profile_email() {
    let usage = commands::usage::parse_gemini_usage(&sample_usage_raw()).unwrap();
    let rendered = commands::usage::format_usage_text("profile-2", "user@example.com", &usage);

    assert!(rendered.starts_with("profile-2  user@example.com\n"));
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn quick_text_logs_gemini_and_gpt_with_remaining_reset() {
    let usage = commands::usage::parse_gemini_usage(&sample_usage_raw()).unwrap();
    let now = Utc.with_ymd_and_hms(2026, 9, 28, 4, 58, 39).unwrap();
    let rendered =
        commands::usage::format_usage_text_at("profile-2", "user@example.com", &usage, now);

    assert!(rendered.contains("profile-2  user@example.com"));
    assert!(rendered.contains("Gemini"));
    assert!(rendered.contains("GPT"));
    assert!(rendered.contains("79%"));
    assert!(rendered.contains("42%"));
    assert!(rendered.contains("88%"));
    assert!(rendered.contains("100%"));
    assert!(rendered.contains("còn 2 giờ"));
    assert!(rendered.contains("còn 3 ngày 12 giờ 28 phút"));
    assert!(rendered.contains("còn 2 giờ 29 phút"));
    assert!(rendered.contains("còn 6 ngày 21 giờ 29 phút"));
    assert!(!rendered.contains("GMT+7"));
    assert!(!rendered.contains("2026"));
    assert!(!rendered.contains("Claude"));
}

#[test]
fn quick_report_formats_multiple_profiles() {
    let raw_one = concat!(
        "Gemini Models\tWeekly Limit Remaining\t50%\t2026-09-30T03:07:10Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t100%\t2026-09-28T06:58:29Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t90%\t2026-10-02T03:07:10Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t70%\t2026-09-28T08:58:29Z\n",
    );
    let report = commands::usage::format_usage_report(&[
        (
            "profile-1".to_string(),
            "one@example.com".to_string(),
            commands::usage::parse_gemini_usage(raw_one).unwrap(),
        ),
        (
            "profile-2".to_string(),
            "two@example.com".to_string(),
            commands::usage::parse_gemini_usage(&sample_usage_raw()).unwrap(),
        ),
    ]);

    assert!(report.contains("profile-1  one@example.com"));
    assert!(report.contains("profile-2  two@example.com"));
    assert!(report.contains("79%"));
    assert!(report.contains("42%"));
    assert!(report.contains("Gemini"));
    assert!(report.contains("GPT"));
    assert!(report.contains("reset"));
}

#[test]
fn json_contract_includes_profile_email_field() {
    let source = include_str!("../src/commands/usage.rs");
    assert!(source.contains("email: String"));
    assert!(source.contains("email: email.to_string()"));
}

#[test]
fn usage_command_and_q_alias_are_exposed() {
    let bin = env!("CARGO_BIN_EXE_agyp");
    for subcommand in ["usage", "q"] {
        let output = std::process::Command::new(bin)
            .args([subcommand, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Usage: agyp usage [OPTIONS] [PROFILE]"));
        assert!(stdout.contains("omit to check all profiles"));
        assert!(stdout.contains("--hide-gpt"));
    }
}

#[test]
fn rejects_duplicate_or_missing_windows() {
    let duplicate = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tWeekly Limit Remaining\t43%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(duplicate).is_err());

    let missing_gpt = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(missing_gpt).is_err());
}

#[test]
fn disabled_window_reports_zero_remaining() {
    let raw = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\tdisabled\n",
        "Claude and GPT models\tWeekly Limit Remaining\tunavailable\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    );
    let usage = commands::usage::parse_gemini_usage(raw).unwrap();
    assert_eq!(usage.gemini.five_hour.remaining_percent, 0);
    assert!(usage.gemini.five_hour.reset_at.is_none());
    assert_eq!(usage.gpt.five_day.remaining_percent, 0);
    assert!(usage.gpt.five_day.reset_at.is_some());
    assert_eq!(usage.gpt.five_hour.remaining_percent, 88);
}

#[test]
fn rejects_invalid_percentage_and_timestamp() {
    let bad_percent = concat!(
        "Gemini Models\tWeekly Limit Remaining\t101%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(bad_percent).is_err());

    let bad_time = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\tnot-a-time\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t88%\t2026-09-28T07:27:35Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(bad_time).is_err());
}
