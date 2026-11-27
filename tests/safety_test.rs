use std::time::{Duration, Instant};

use agyp::commands::quota::{calculate_quota_exit_code, classify_probe_outcome, QuotaStatus};
use agyp::commands::task::{classify_task_outcome, TaskFailureKind};
use agyp::commands::usage::UsageCache;
use agyp::safety::{
    acquire_launch_permit, clear_suspension, compute_launch_wait, is_tos_suspension_text,
    list_suspensions, mark_suspended, pseudo_random_jitter, read_suspension, LaunchError,
    SafetySettings, SUSPENDED_EXIT_CODE,
};

const TOS_LINE: &str = "Error: 403 This service has been disabled in this account for violation of Terms of Service. Please submit an appeal to continue using this product.";

fn no_pacing(max_concurrent: usize) -> SafetySettings {
    SafetySettings {
        max_concurrent,
        launch_gap: Duration::ZERO,
        launch_jitter: Duration::ZERO,
    }
}

#[test]
fn detects_tos_suspension_messages() {
    assert!(is_tos_suspension_text(TOS_LINE));
    assert!(is_tos_suspension_text(
        "Service disabled for violation of the Terms of Service (Code 403)"
    ));
    assert!(is_tos_suspension_text(
        "this service has been disabled. please submit an appeal"
    ));
    assert!(!is_tos_suspension_text("Error: 429 RESOURCE_EXHAUSTED"));
    assert!(!is_tos_suspension_text("Individual quota reached"));
    assert!(!is_tos_suspension_text(
        "Read the terms of service in docs/"
    ));
}

#[test]
fn settings_read_env_overrides_and_keep_defaults_on_garbage() {
    let defaults = SafetySettings::from_lookup(|_| None);
    assert_eq!(defaults, SafetySettings::default());
    assert_eq!(defaults.max_concurrent, 4);

    let custom = SafetySettings::from_lookup(|key| match key {
        "AGYP_MAX_CONCURRENT" => Some("2".into()),
        "AGYP_LAUNCH_GAP_SECS" => Some("0".into()),
        "AGYP_LAUNCH_JITTER_SECS" => Some("nope".into()),
        _ => None,
    });
    assert_eq!(custom.max_concurrent, 2);
    assert_eq!(custom.launch_gap, Duration::ZERO);
    assert_eq!(
        custom.launch_jitter,
        SafetySettings::default().launch_jitter
    );
}

#[test]
fn launch_wait_respects_gap_and_jitter() {
    let gap = Duration::from_secs(15);
    let jitter = Duration::from_secs(5);
    assert_eq!(
        compute_launch_wait(None, 1_000_000, gap, jitter),
        Duration::ZERO
    );
    // Previous launch 4 s ago: wait the remaining 16 s.
    assert_eq!(
        compute_launch_wait(Some(996_000), 1_000_000, gap, jitter),
        Duration::from_secs(16)
    );
    // Previous launch long ago: no wait.
    assert_eq!(
        compute_launch_wait(Some(1), 1_000_000, gap, jitter),
        Duration::ZERO
    );
}

#[test]
fn jitter_stays_in_range() {
    assert_eq!(pseudo_random_jitter(Duration::ZERO), Duration::ZERO);
    for _ in 0..200 {
        assert!(pseudo_random_jitter(Duration::from_secs(10)) <= Duration::from_secs(10));
    }
}

#[test]
fn suspension_marker_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_suspension(dir.path(), "p1").is_none());
    mark_suspended(dir.path(), "p1", "task", Some(TOS_LINE)).unwrap();
    let record = read_suspension(dir.path(), "p1").expect("marker");
    assert_eq!(record.profile, "p1");
    assert_eq!(record.source, "task");
    assert_eq!(list_suspensions(dir.path()).len(), 1);
    assert!(clear_suspension(dir.path(), "p1").unwrap());
    assert!(!clear_suspension(dir.path(), "p1").unwrap());
    assert!(read_suspension(dir.path(), "p1").is_none());
}

#[test]
fn suspension_marker_rejects_path_traversal() {
    let dir = tempfile::tempdir().unwrap();
    assert!(mark_suspended(dir.path(), "../evil", "manual", None).is_err());
}

#[test]
fn suspended_profile_is_refused_and_others_are_not() {
    let dir = tempfile::tempdir().unwrap();
    mark_suspended(dir.path(), "banned", "manual", None).unwrap();
    match acquire_launch_permit(dir.path(), "banned", &no_pacing(4)) {
        Err(err @ LaunchError::Suspended(_)) => {
            assert_eq!(err.exit_code(), SUSPENDED_EXIT_CODE);
            assert!(err.message().contains("agyp unsuspend banned"));
        }
        other => panic!("expected suspension refusal, got {other:?}"),
    }
    assert!(acquire_launch_permit(dir.path(), "healthy", &no_pacing(4)).is_ok());
}

#[test]
fn concurrency_cap_blocks_until_a_slot_frees() {
    let dir = tempfile::tempdir().unwrap();
    let first = acquire_launch_permit(dir.path(), "a", &no_pacing(1)).unwrap();
    let path = dir.path().to_path_buf();
    let handle = std::thread::spawn(move || {
        let started = Instant::now();
        let _second = acquire_launch_permit(&path, "b", &no_pacing(1)).unwrap();
        started.elapsed()
    });
    std::thread::sleep(Duration::from_millis(600));
    drop(first);
    let waited = handle.join().unwrap();
    assert!(
        waited >= Duration::from_millis(500),
        "waited only {waited:?}"
    );
}

#[test]
fn zero_cap_means_unlimited() {
    let dir = tempfile::tempdir().unwrap();
    let _a = acquire_launch_permit(dir.path(), "a", &no_pacing(0)).unwrap();
    let _b = acquire_launch_permit(dir.path(), "b", &no_pacing(0)).unwrap();
}

#[test]
fn pacing_spaces_consecutive_launches() {
    let dir = tempfile::tempdir().unwrap();
    let settings = SafetySettings {
        max_concurrent: 0,
        launch_gap: Duration::from_secs(1),
        launch_jitter: Duration::ZERO,
    };
    let started = Instant::now();
    drop(acquire_launch_permit(dir.path(), "a", &settings).unwrap());
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "first launch must not wait"
    );
    drop(acquire_launch_permit(dir.path(), "b", &settings).unwrap());
    assert!(started.elapsed() >= Duration::from_millis(950));
}

#[test]
fn task_and_quota_classify_tos_as_suspended() {
    assert_eq!(
        classify_task_outcome(false, Some(1), "", TOS_LINE),
        Err(TaskFailureKind::AccountSuspended)
    );
    // Even when the 403 text arrives together with a quota line.
    let mixed = format!("Error: 429 RESOURCE_EXHAUSTED\n{TOS_LINE}");
    assert_eq!(
        classify_task_outcome(false, Some(1), "", &mixed),
        Err(TaskFailureKind::AccountSuspended)
    );
    assert_eq!(
        TaskFailureKind::AccountSuspended.as_str(),
        "account_suspended"
    );

    let (status, _) = classify_probe_outcome(false, Some(1), "", TOS_LINE);
    assert_eq!(status, QuotaStatus::Suspended);
    assert_eq!(status.as_str(), "suspended");
    assert_eq!(calculate_quota_exit_code(&[QuotaStatus::Suspended]), 2);
}

#[test]
fn usage_cache_entries_expire() {
    let now = chrono::Utc::now();
    let mut cache = UsageCache::default();
    cache.entries.insert(
        "p1".into(),
        agyp::commands::usage::UsageCacheEntry {
            observed_at: now - chrono::Duration::seconds(60),
            raw: "raw".into(),
        },
    );
    let ttl = Duration::from_secs(180);
    assert_eq!(cache.fresh_raw("p1", now, ttl), Some("raw"));
    assert_eq!(
        cache.fresh_raw("p1", now + chrono::Duration::seconds(200), ttl),
        None
    );
    assert_eq!(cache.fresh_raw("p2", now, ttl), None);
}
