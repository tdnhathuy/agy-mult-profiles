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

#[test]
fn parses_only_gemini_usage_rows() {
    let raw = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
        "Claude and GPT models\tFive Hour Limit Remaining\t100%\t2026-09-28T07:27:35Z\n",
    );

    let usage = commands::usage::parse_gemini_usage(raw).unwrap();
    assert_eq!(usage.weekly.remaining_percent, 42);
    assert_eq!(usage.five_hour.remaining_percent, 79);
    assert_eq!(
        usage.weekly.reset_at.to_rfc3339(),
        "2026-10-01T17:26:35+00:00"
    );
    assert_eq!(
        usage.five_hour.reset_at.to_rfc3339(),
        "2026-09-28T06:58:39+00:00"
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
    );

    let usage = commands::usage::parse_gemini_usage(raw).unwrap();
    assert_eq!(usage.weekly.remaining_percent, 42);
    assert_eq!(usage.five_hour.remaining_percent, 79);
}

#[test]
fn vietnam_rfc3339_is_machine_readable_with_plus_seven_offset() {
    let dt = Utc.with_ymd_and_hms(2026, 10, 1, 17, 26, 35).unwrap();
    assert_eq!(
        commands::usage::format_vietnam_rfc3339(dt),
        "2026-10-02T00:26:35+07:00"
    );
}

#[test]
fn quick_text_omits_claude_and_gpt() {
    let raw = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
        "Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-10-05T02:27:35Z\n",
    );
    let usage = commands::usage::parse_gemini_usage(raw).unwrap();
    let rendered = commands::usage::format_usage_text("profile-2", &usage);

    assert!(rendered.contains("profile-2"));
    assert!(rendered.contains("5 giờ: 79%"));
    assert!(rendered.contains("Hàng tuần: 42%"));
    assert!(!rendered.contains("Claude"));
    assert!(!rendered.contains("GPT"));
    assert!(rendered.contains("GMT+7"));
}

#[test]
fn quick_report_formats_multiple_profiles() {
    let raw_one = concat!(
        "Gemini Models\tWeekly Limit Remaining\t50%\t2026-09-30T03:07:10Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t100%\t2026-09-28T06:58:29Z\n",
    );
    let raw_two = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
    );
    let report = commands::usage::format_usage_report(&[
        (
            "profile-1".to_string(),
            commands::usage::parse_gemini_usage(raw_one).unwrap(),
        ),
        (
            "profile-2".to_string(),
            commands::usage::parse_gemini_usage(raw_two).unwrap(),
        ),
    ]);

    assert!(report.contains("profile-1"));
    assert!(report.contains("profile-2"));
    assert!(report.contains("5 giờ: 79%"));
    assert!(report.contains("Hàng tuần: 42%"));
    assert!(!report.contains("Claude"));
    assert!(!report.contains("GPT"));
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
    }
}

#[test]
fn rejects_duplicate_or_missing_gemini_windows() {
    let duplicate = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tWeekly Limit Remaining\t43%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(duplicate).is_err());

    let missing = "Gemini Models\tWeekly Limit Remaining\t42%\t2026-10-01T17:26:35Z\n";
    assert!(commands::usage::parse_gemini_usage(missing).is_err());
}

#[test]
fn rejects_invalid_percentage_and_timestamp() {
    let bad_percent = concat!(
        "Gemini Models\tWeekly Limit Remaining\t101%\t2026-10-01T17:26:35Z\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(bad_percent).is_err());

    let bad_time = concat!(
        "Gemini Models\tWeekly Limit Remaining\t42%\tnot-a-time\n",
        "Gemini Models\tFive Hour Limit Remaining\t79%\t2026-09-28T06:58:39Z\n",
    );
    assert!(commands::usage::parse_gemini_usage(bad_time).is_err());
}
