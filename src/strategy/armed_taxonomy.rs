//! 2026-04-19 PR #2.f: armed watch 관련 phase / event_log type / outcome label /
//! detail reason 의 **single source of truth**.
//!
//! 여러 파일에 흩어진 문자열 리터럴이 있으면 taxonomy 가 쉽게 깨진다. 이 모듈은
//! `docs/monitoring/execution-journal-taxonomy.md` §1.6 과 1:1 대응하는 상수 + cross-check
//! 테스트 + metadata validator 를 제공한다.
//!
//! # 사용 규칙
//!
//! - `live_runner.rs` 가 armed watch journal / event_log 에 쓰는 phase/type 문자열은
//!   전부 이 모듈 상수를 참조해야 한다.
//! - `ArmedWaitOutcome::label()` 은 여기의 `OUTCOME_*` 와 정확히 일치해야 한다
//!   (`outcome_labels_match_armed_wait_outcome` 테스트가 이를 강제).
//! - 새 outcome / detail 을 추가하면 상수 + taxonomy 문서 + validator 테스트를 **같은 PR**
//!   에서 수정한다.

// ── Journal phase (execution_journal.phase) ───────────────────────────────

pub const JOURNAL_PHASE_ENTER: &str = "armed_wait_enter";
pub const JOURNAL_PHASE_EXIT: &str = "armed_wait_exit";

// ── event_log.event_type ───────────────────────────────────────────────────

pub const EVENT_LOG_TYPE_ENTER: &str = "armed_watch_enter";
pub const EVENT_LOG_TYPE_EXIT: &str = "armed_watch_exit";
pub const EVENT_LOG_CATEGORY: &str = "strategy";

// ── ArmedWaitOutcome::label() 과 동일해야 하는 outcome 라벨 ────────────────

pub const OUTCOME_READY: &str = "ready";
pub const OUTCOME_CUTOFF_REACHED: &str = "cutoff_reached";
pub const OUTCOME_PREFLIGHT_FAILED: &str = "preflight_failed";
pub const OUTCOME_DRIFT_EXCEEDED: &str = "drift_exceeded";
pub const OUTCOME_STOPPED: &str = "stopped";
pub const OUTCOME_ABORTED: &str = "aborted";
pub const OUTCOME_ALREADY_ARMED: &str = "already_armed";

pub const ALL_OUTCOME_LABELS: &[&str] = &[
    OUTCOME_READY,
    OUTCOME_CUTOFF_REACHED,
    OUTCOME_PREFLIGHT_FAILED,
    OUTCOME_DRIFT_EXCEEDED,
    OUTCOME_STOPPED,
    OUTCOME_ABORTED,
    OUTCOME_ALREADY_ARMED,
];

// ── PreflightFailed(detail) — preflight gate 실패 사유 ────────────────────

pub const DETAIL_PREFLIGHT_SUBSCRIPTION_STALE: &str = "armed_preflight_subscription_stale";
pub const DETAIL_PREFLIGHT_SPREAD_EXCEEDED: &str = "armed_preflight_spread_exceeded";
pub const DETAIL_PREFLIGHT_NAV_FAILED: &str = "armed_preflight_nav_failed";
pub const DETAIL_PREFLIGHT_REGIME_UNCONFIRMED: &str = "armed_preflight_regime_unconfirmed";
pub const DETAIL_PREFLIGHT_GATE: &str = "armed_preflight_gate";

pub const ALL_PREFLIGHT_DETAILS: &[&str] = &[
    DETAIL_PREFLIGHT_SUBSCRIPTION_STALE,
    DETAIL_PREFLIGHT_SPREAD_EXCEEDED,
    DETAIL_PREFLIGHT_NAV_FAILED,
    DETAIL_PREFLIGHT_REGIME_UNCONFIRMED,
    DETAIL_PREFLIGHT_GATE,
];

// ── Aborted(detail) — manual/degraded 전이 사유 ────────────────────────────

pub const DETAIL_ABORTED_MANUAL_INTERVENTION: &str = "armed_manual_intervention";
pub const DETAIL_ABORTED_DEGRADED: &str = "armed_degraded";
pub const DETAIL_ABORTED_ENTRY_SUBMIT_TIMEOUT: &str = "armed_entry_submit_timeout";

pub const ALL_ABORTED_DETAILS: &[&str] = &[
    DETAIL_ABORTED_MANUAL_INTERVENTION,
    DETAIL_ABORTED_DEGRADED,
    DETAIL_ABORTED_ENTRY_SUBMIT_TIMEOUT,
];

// ── 조회 helper ───────────────────────────────────────────────────────────

pub fn is_known_outcome_label(label: &str) -> bool {
    ALL_OUTCOME_LABELS.contains(&label)
}

pub fn is_known_preflight_detail(detail: &str) -> bool {
    ALL_PREFLIGHT_DETAILS.contains(&detail)
}

pub fn is_known_aborted_detail(detail: &str) -> bool {
    ALL_ABORTED_DETAILS.contains(&detail)
}

// ── metadata validator (pure, I/O 없음) ───────────────────────────────────

/// `armed_wait_exit.metadata` 필수 키 검증. 빈 Vec 이면 통과.
pub fn validate_armed_exit_metadata(metadata: &serde_json::Value) -> Vec<&'static str> {
    const REQUIRED: &[&str] = &["outcome", "detail", "severity", "duration_ms"];
    REQUIRED
        .iter()
        .filter_map(|key| metadata.get(*key).is_none().then_some(*key))
        .collect()
}

/// `armed_wait_enter.metadata` 필수 키 검증. 빈 Vec 이면 통과.
pub fn validate_armed_enter_metadata(metadata: &serde_json::Value) -> Vec<&'static str> {
    const REQUIRED: &[&str] = &["budget_ms", "tick_interval_ms", "signal_entry_price"];
    REQUIRED
        .iter()
        .filter_map(|key| metadata.get(*key).is_none().then_some(*key))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::live_runner::ArmedWaitOutcome;

    // ── outcome label cross-check — taxonomy 의 single source of truth 보장 ──

    #[test]
    fn outcome_labels_match_armed_wait_outcome() {
        // ArmedWaitOutcome::label() 이 반환하는 모든 값이 상수 집합과 같아야 한다.
        // 어느 한쪽을 바꾸면 이 테스트가 실패 → taxonomy 재확인 강제.
        let cases = [
            (ArmedWaitOutcome::Ready, OUTCOME_READY),
            (ArmedWaitOutcome::CutoffReached, OUTCOME_CUTOFF_REACHED),
            (
                ArmedWaitOutcome::PreflightFailed("x"),
                OUTCOME_PREFLIGHT_FAILED,
            ),
            (ArmedWaitOutcome::DriftExceeded, OUTCOME_DRIFT_EXCEEDED),
            (ArmedWaitOutcome::Stopped, OUTCOME_STOPPED),
            (ArmedWaitOutcome::Aborted("y"), OUTCOME_ABORTED),
            (ArmedWaitOutcome::AlreadyArmed, OUTCOME_ALREADY_ARMED),
        ];
        for (outcome, expected) in cases {
            assert_eq!(
                outcome.label(),
                expected,
                "ArmedWaitOutcome::label() 와 상수 불일치: {:?}",
                outcome
            );
            assert!(
                is_known_outcome_label(outcome.label()),
                "label {:?} 가 ALL_OUTCOME_LABELS 에 없음",
                outcome.label()
            );
        }
    }

    #[test]
    fn all_outcome_labels_are_unique() {
        let mut labels: Vec<&'static str> = ALL_OUTCOME_LABELS.to_vec();
        let total = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), total, "outcome label 중복");
    }

    #[test]
    fn preflight_detail_lookup_recognizes_defined_values() {
        for d in ALL_PREFLIGHT_DETAILS {
            assert!(is_known_preflight_detail(d), "{}", d);
        }
        assert!(!is_known_preflight_detail("random_unknown"));
    }

    #[test]
    fn aborted_detail_lookup_recognizes_defined_values() {
        for d in ALL_ABORTED_DETAILS {
            assert!(is_known_aborted_detail(d), "{}", d);
        }
        assert!(!is_known_aborted_detail("random_unknown"));
    }

    #[test]
    fn preflight_and_aborted_details_do_not_overlap() {
        for p in ALL_PREFLIGHT_DETAILS {
            assert!(
                !ALL_ABORTED_DETAILS.contains(p),
                "preflight detail {} 이 aborted detail 집합에도 존재",
                p
            );
        }
    }

    // ── metadata validator ─────────────────────────────────────────────────

    #[test]
    fn exit_validator_accepts_complete_metadata() {
        let md = serde_json::json!({
            "outcome": OUTCOME_READY,
            "detail": "",
            "severity": "info",
            "duration_ms": 180,
        });
        let missing = validate_armed_exit_metadata(&md);
        assert!(missing.is_empty(), "unexpected missing: {:?}", missing);
    }

    #[test]
    fn exit_validator_reports_all_missing_keys() {
        let md = serde_json::json!({});
        let missing = validate_armed_exit_metadata(&md);
        assert!(missing.contains(&"outcome"));
        assert!(missing.contains(&"detail"));
        assert!(missing.contains(&"severity"));
        assert!(missing.contains(&"duration_ms"));
    }

    #[test]
    fn exit_validator_detects_single_missing_key() {
        let md = serde_json::json!({
            "outcome": OUTCOME_READY,
            "detail": "",
            "severity": "info",
            // duration_ms 누락
        });
        let missing = validate_armed_exit_metadata(&md);
        assert_eq!(missing, vec!["duration_ms"]);
    }

    #[test]
    fn enter_validator_accepts_complete_metadata() {
        let md = serde_json::json!({
            "budget_ms": 200,
            "tick_interval_ms": 150,
            "signal_entry_price": 10_000,
        });
        let missing = validate_armed_enter_metadata(&md);
        assert!(missing.is_empty(), "unexpected missing: {:?}", missing);
    }

    #[test]
    fn enter_validator_reports_missing_budget() {
        let md = serde_json::json!({
            "tick_interval_ms": 150,
            "signal_entry_price": 10_000,
        });
        let missing = validate_armed_enter_metadata(&md);
        assert_eq!(missing, vec!["budget_ms"]);
    }
}
