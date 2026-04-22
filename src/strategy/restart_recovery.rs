//! 2026-04-19 PR #3.b (execution-followup-plan §3): 재시작 복구 분기 판정 pure function.
//!
//! # 목적
//!
//! `LiveRunner::check_and_restore_position` 의 **분기 규칙** 을 입력/출력만 가진
//! 순수 함수로 뽑아 매트릭스 테스트로 고정한다. 실제 I/O (DB 조회, KIS 주문 조회,
//! event_log 기록, cancel_all_pending_orders 호출) 는 이 파일에 들어오지 않는다.
//!
//! # 사용 규칙 (계획서 수용 기준)
//!
//! - 이 파일은 `check_and_restore_position` 의 현재 판정 기준을 **미러링** 한다.
//! - `live_runner.rs` 의 복구 분기 규칙이 바뀌면 이 파일도 같은 PR 에서 수정해야
//!   taxonomy 가 깨지지 않는다.
//! - 실제 행동 (포지션 복구, TP 재발주, manual 전환 등) 은 live_runner 가 한다.
//!   이 함수는 "어느 분기를 타야 하는가" 만 결정한다.
//!
//! # 이 모듈에서 하지 않는 일
//!
//! - DB/네트워크 I/O 및 비동기.
//! - `check_and_restore_position` 전체를 리팩터링해 이 함수를 호출하게 만드는 것
//!   (계획서 §5 실행 actor 도입 준비 단계와 맞물리므로 후속 PR 에서 판단).
//!
//! # PR #3.d 추가
//!
//! `RestartAction::journal_phase()` 로 `execution_journal` taxonomy phase 문자열을
//! 노출한다. `label()` 이 내부 분류 라벨 (action 이름) 이라면, `journal_phase()` 는
//! `docs/monitoring/execution-journal-taxonomy.md` §1.3 에 명시된 phase 이름이다.
//! 두 값이 달라지는 이유는 한 taxonomy phase 가 여러 action 에 대응할 수 있기
//! 때문이다 (예: `entry_restart_manual_intervention` 은 `EntryPendingManualRestore`
//! 와 `EntryPendingManualOnQueryFail` 모두 사용).

use crate::strategy::types::PositionSide;

/// DB `active_positions` 에서 읽은 저장 실행 상태의 의미 단위 스냅샷.
///
/// 실제 DB 스키마의 모든 컬럼을 담지 않는다 — 판정에 쓰이는 값만. 구조체 필드가
/// 늘어나면 분기 규칙이 달라졌다는 신호.
#[derive(Debug, Clone, PartialEq)]
pub enum SavedExecutionState {
    /// DB 에 행은 있지만 상태가 flat 으로 저장됨 (정상 상황에서는 행 자체가 없음).
    Flat,
    EntryPending {
        pending_entry_order_no: String,
        side: PositionSide,
        entry_price: i64,
        quantity: u64,
    },
    /// 진입 부분 체결 — 일부 수량 잔고에 있음.
    EntryPartial {
        pending_entry_order_no: String,
        side: PositionSide,
        entry_price: i64,
        quantity: u64,
    },
    Open {
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
        quantity: u64,
        tp_order_no: Option<String>,
    },
    ExitPending {
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
        quantity: u64,
    },
    ExitPartial {
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
        quantity: u64,
    },
    /// 이전에 manual 로 전환되어 DB 에 고정된 상태. 재기동 시 그대로 복원.
    ManualIntervention {
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
        quantity: u64,
    },
}

/// KIS 잔고 조회 결과의 요약.
#[derive(Debug, Clone, PartialEq)]
pub enum HoldingsSnapshot {
    /// 잔고 0 주.
    None,
    /// 잔고 있음.
    Present { qty: u64, avg_price: Option<i64> },
    /// 조회 자체가 실패 (네트워크/파싱). fail-safe 로 manual 전환.
    LookupFailed { reason: String },
}

/// 특정 주문번호에 대한 체결 조회 결과 요약.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecutionQuerySummary {
    /// 조회 대상 주문 없음 (예: EntryPending 이 아닌 상태).
    NotApplicable,
    /// 주문이 체결됨 (full or partial).
    Filled { qty: u64, avg_price: i64 },
    /// 아직 체결 안 됨 (대기/취소 가능).
    Unfilled,
    /// 주문 이미 취소됨.
    Cancelled,
    /// 조회 실패 (REST 에러 등).
    QueryFailed { reason: String },
}

/// `classify_restart_action` 에 필요한 모든 관측값 묶음.
#[derive(Debug, Clone, PartialEq)]
pub struct RestartInput {
    pub saved_state: Option<SavedExecutionState>,
    pub holdings_snapshot: HoldingsSnapshot,
    pub execution_query: ExecutionQuerySummary,
    /// 현재 KIS 에 미체결 주문이 존재하는지 (다른 경로로 관측된 경우).
    /// 이 값은 현재 판정에 직접 쓰이지 않지만, 향후 `OrphanCleanup` 분기
    /// 정교화에 대비해 보존.
    pub has_stale_pending_orders: bool,
}

/// 재시작 복구 시 수행해야 할 행동 분류.
///
/// `label()` 은 event_log/journal 에 그대로 기록할 수 있는 문자열.
#[derive(Debug, Clone, PartialEq)]
pub enum RestartAction {
    /// saved=None + 잔고 없음 → `cancel_all_pending_orders` 후 flat.
    OrphanCleanup,
    /// saved=None + 잔고 있음 → manual (DB 복구 불가).
    OrphanHoldingDetected { qty: u64, avg_price: Option<i64> },
    /// 잔고 조회 실패 → fail-safe manual.
    ManualFailSafe { reason: String },
    /// saved=Manual → 그대로 복원.
    ManualRestore { qty: u64 },
    /// saved=EntryPending/EntryPartial + 잔고 있음 → 포지션 재구성 + manual.
    EntryPendingManualRestore { qty: u64 },
    /// saved=EntryPending + 잔고 없음 → 주문 취소 + flat (`entry_restart_auto_clear`).
    EntryPendingAutoClear,
    /// saved=EntryPending + 주문 조회 실패 → manual (불확실 상태, fail-safe).
    EntryPendingManualOnQueryFail { reason: String },
    /// saved=ExitPending/ExitPartial + 잔고 있음 → manual.
    ExitPendingManualRestore { qty: u64 },
    /// saved=ExitPending/ExitPartial + 잔고 없음 → auto flat (`exit_restart_auto_flat`).
    ExitPendingAutoFlat,
    /// saved=Open + 잔고 있음 → 포지션 재구성 + 새 TP 지정가 발주.
    OpenPositionRestore {
        qty: u64,
        side: PositionSide,
        entry_price: i64,
        stop_loss: i64,
        take_profit: i64,
    },
    /// saved=Open + 잔고 없음 → 유령 포지션 정리 (flat).
    OpenPhantomCleanup,
}

impl RestartAction {
    /// event_log / execution_journal 용 라벨.
    pub fn label(&self) -> &'static str {
        match self {
            RestartAction::OrphanCleanup => "orphan_cleanup",
            RestartAction::OrphanHoldingDetected { .. } => "orphan_holding_detected",
            RestartAction::ManualFailSafe { .. } => "manual_fail_safe",
            RestartAction::ManualRestore { .. } => "manual_restore",
            RestartAction::EntryPendingManualRestore { .. } => "entry_pending_manual_restore",
            RestartAction::EntryPendingAutoClear => "entry_pending_auto_clear",
            RestartAction::EntryPendingManualOnQueryFail { .. } => {
                "entry_pending_manual_on_query_fail"
            }
            RestartAction::ExitPendingManualRestore { .. } => "exit_pending_manual_restore",
            RestartAction::ExitPendingAutoFlat => "exit_pending_auto_flat",
            RestartAction::OpenPositionRestore { .. } => "open_position_restore",
            RestartAction::OpenPhantomCleanup => "open_phantom_cleanup",
        }
    }

    /// manual 전환이 필요한 분기인지.
    pub fn requires_manual_intervention(&self) -> bool {
        matches!(
            self,
            RestartAction::OrphanHoldingDetected { .. }
                | RestartAction::ManualFailSafe { .. }
                | RestartAction::ManualRestore { .. }
                | RestartAction::EntryPendingManualRestore { .. }
                | RestartAction::EntryPendingManualOnQueryFail { .. }
                | RestartAction::ExitPendingManualRestore { .. }
        )
    }

    /// `execution_journal.phase` 에 기록할 taxonomy 문자열.
    ///
    /// `docs/monitoring/execution-journal-taxonomy.md` §1.3 "재시작 복구 (복구 단위)"
    /// 의 phase 이름과 1:1 (또는 N:1) 대응. 코드 상 새 phase 추가/변경 시 taxonomy
    /// 문서도 같은 PR 에서 수정해야 한다 (§5 변경 프로토콜).
    pub fn journal_phase(&self) -> &'static str {
        match self {
            RestartAction::OrphanCleanup => "orphan_restart_cleanup",
            RestartAction::OrphanHoldingDetected { .. } => "orphan_restart_holding_detected",
            RestartAction::ManualFailSafe { .. } => "restart_balance_lookup_failed_manual",
            RestartAction::ManualRestore { .. } => "manual_restart_restore",
            RestartAction::EntryPendingManualRestore { .. }
            | RestartAction::EntryPendingManualOnQueryFail { .. } => {
                "entry_restart_manual_intervention"
            }
            RestartAction::EntryPendingAutoClear => "entry_restart_auto_clear",
            RestartAction::ExitPendingManualRestore { .. } => "exit_restart_manual_intervention",
            RestartAction::ExitPendingAutoFlat => "exit_restart_auto_flat",
            RestartAction::OpenPositionRestore { .. } => "open_restart_restore",
            RestartAction::OpenPhantomCleanup => "open_restart_phantom_cleanup",
        }
    }
}

/// 2026-04-19 PR #3.f: 재시작 manual phase 의 `metadata` JSON 이
/// `execution-journal-taxonomy.md` §1.5 "Manual 분기 필수 메타 표준" 을 만족하는지
/// 검증한다.
///
/// 반환값이 빈 Vec 이면 통과. 누락된 키 이름을 `&'static str` 배열로 반환.
/// I/O 없음 — 단위 테스트 및 debug assertion 용도.
///
/// 호출측이 manual 기록 직전에 assert 하거나 운영 쿼리로 결락 찾아낼 때 사용.
/// 자동화된 runtime enforcement 는 현재 단계에서는 하지 않는다 (PR #3.g 이후 검토).
pub fn validate_restart_manual_metadata(
    phase: &str,
    metadata: &serde_json::Value,
) -> Vec<&'static str> {
    let mut missing: Vec<&'static str> = Vec::new();

    // 공통 필수 키 — taxonomy §1.5.
    const COMMON: &[&str] = &["prior_execution_state", "manual_category"];
    for key in COMMON {
        if metadata.get(key).is_none() {
            missing.push(*key);
        }
    }

    // phase-specific 필수 키.
    let specific: &[&'static str] = match phase {
        "entry_restart_manual_intervention" => &["pending_entry_order_no"],
        "exit_restart_manual_intervention" => &["last_exit_order_no", "last_exit_reason"],
        "orphan_restart_holding_detected" => &["quantity"],
        "manual_restart_restore" => &["stop_loss", "take_profit"],
        "restart_balance_lookup_failed_manual" => &[],
        _ => &[],
    };
    for key in specific {
        if metadata.get(key).is_none() {
            missing.push(*key);
        }
    }

    missing
}

/// 저장 상태 × 잔고 × 주문조회 의 조합으로 재시작 복구 행동을 판정.
///
/// 순서 규칙 (우선순위):
/// 1. 잔고 조회 실패 → `ManualFailSafe` (saved 유무와 무관).
/// 2. `ManualIntervention` 저장 → `ManualRestore` (다른 힌트보다 우선 — 수동 보존).
/// 3. saved 별 분기 + 잔고 유무.
///
/// saved=None 경로는 1/2 가 적용 안 되면 잔고 유무만으로 결정.
pub fn classify_restart_action(input: &RestartInput) -> RestartAction {
    // 1. 잔고 조회 실패는 항상 manual fail-safe.
    if let HoldingsSnapshot::LookupFailed { reason } = &input.holdings_snapshot {
        return RestartAction::ManualFailSafe {
            reason: reason.clone(),
        };
    }

    let holdings = &input.holdings_snapshot;

    // 2. Manual 저장 → 그대로 복원 (잔고 힌트 무시 — 운영자 보호).
    if let Some(SavedExecutionState::ManualIntervention { quantity, .. }) = &input.saved_state {
        return RestartAction::ManualRestore { qty: *quantity };
    }

    match (&input.saved_state, holdings) {
        // saved=None: DB 에 없음 → orphan 기준.
        (None, HoldingsSnapshot::None) => RestartAction::OrphanCleanup,
        (None, HoldingsSnapshot::Present { qty, avg_price }) => {
            RestartAction::OrphanHoldingDetected {
                qty: *qty,
                avg_price: *avg_price,
            }
        }

        // saved=Flat: DB 레코드는 있지만 flat — saved=None 과 동일하게 처리.
        (Some(SavedExecutionState::Flat), HoldingsSnapshot::None) => RestartAction::OrphanCleanup,
        (Some(SavedExecutionState::Flat), HoldingsSnapshot::Present { qty, avg_price }) => {
            RestartAction::OrphanHoldingDetected {
                qty: *qty,
                avg_price: *avg_price,
            }
        }

        // EntryPending / EntryPartial: 잔고 있으면 manual, 없으면 auto clear.
        (
            Some(
                SavedExecutionState::EntryPending { .. } | SavedExecutionState::EntryPartial { .. },
            ),
            HoldingsSnapshot::Present { qty, .. },
        ) => RestartAction::EntryPendingManualRestore { qty: *qty },
        (
            Some(
                SavedExecutionState::EntryPending { .. } | SavedExecutionState::EntryPartial { .. },
            ),
            HoldingsSnapshot::None,
        ) => match &input.execution_query {
            ExecutionQuerySummary::QueryFailed { reason } => {
                RestartAction::EntryPendingManualOnQueryFail {
                    reason: reason.clone(),
                }
            }
            _ => RestartAction::EntryPendingAutoClear,
        },

        // ExitPending / ExitPartial
        (
            Some(SavedExecutionState::ExitPending { .. } | SavedExecutionState::ExitPartial { .. }),
            HoldingsSnapshot::Present { qty, .. },
        ) => RestartAction::ExitPendingManualRestore { qty: *qty },
        (
            Some(SavedExecutionState::ExitPending { .. } | SavedExecutionState::ExitPartial { .. }),
            HoldingsSnapshot::None,
        ) => RestartAction::ExitPendingAutoFlat,

        // Open
        (
            Some(SavedExecutionState::Open {
                side,
                entry_price,
                stop_loss,
                take_profit,
                ..
            }),
            HoldingsSnapshot::Present { qty, .. },
        ) => RestartAction::OpenPositionRestore {
            qty: *qty,
            side: *side,
            entry_price: *entry_price,
            stop_loss: *stop_loss,
            take_profit: *take_profit,
        },
        (Some(SavedExecutionState::Open { .. }), HoldingsSnapshot::None) => {
            RestartAction::OpenPhantomCleanup
        }

        // LookupFailed / ManualIntervention 는 함수 상단에서 early return — unreachable.
        (_, HoldingsSnapshot::LookupFailed { .. })
        | (Some(SavedExecutionState::ManualIntervention { .. }), _) => {
            unreachable!("handled by early return at top")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_state(qty: u64) -> SavedExecutionState {
        SavedExecutionState::Open {
            side: PositionSide::Long,
            entry_price: 10_000,
            stop_loss: 9_900,
            take_profit: 10_250,
            quantity: qty,
            tp_order_no: Some("0000000001".to_string()),
        }
    }

    fn entry_pending_state(qty: u64) -> SavedExecutionState {
        SavedExecutionState::EntryPending {
            pending_entry_order_no: "0000000002".to_string(),
            side: PositionSide::Long,
            entry_price: 10_000,
            quantity: qty,
        }
    }

    fn exit_pending_state(qty: u64) -> SavedExecutionState {
        SavedExecutionState::ExitPending {
            side: PositionSide::Long,
            entry_price: 10_000,
            stop_loss: 9_900,
            take_profit: 10_250,
            quantity: qty,
        }
    }

    fn manual_state(qty: u64) -> SavedExecutionState {
        SavedExecutionState::ManualIntervention {
            side: PositionSide::Long,
            entry_price: 10_000,
            stop_loss: 9_900,
            take_profit: 10_250,
            quantity: qty,
        }
    }

    fn input(
        saved: Option<SavedExecutionState>,
        holdings: HoldingsSnapshot,
        query: ExecutionQuerySummary,
    ) -> RestartInput {
        RestartInput {
            saved_state: saved,
            holdings_snapshot: holdings,
            execution_query: query,
            has_stale_pending_orders: false,
        }
    }

    // ── saved=None (Flat 경로) ────────────────────────────────────────────

    #[test]
    fn flat_no_holdings_returns_orphan_cleanup() {
        let r = classify_restart_action(&input(
            None,
            HoldingsSnapshot::None,
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(r, RestartAction::OrphanCleanup);
        assert!(!r.requires_manual_intervention());
    }

    #[test]
    fn flat_with_holdings_returns_orphan_holding_detected() {
        let r = classify_restart_action(&input(
            None,
            HoldingsSnapshot::Present {
                qty: 10,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(
            r,
            RestartAction::OrphanHoldingDetected {
                qty: 10,
                avg_price: Some(10_000)
            }
        );
        assert!(r.requires_manual_intervention());
    }

    #[test]
    fn saved_flat_treated_same_as_none() {
        let r1 = classify_restart_action(&input(
            Some(SavedExecutionState::Flat),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(r1, RestartAction::OrphanCleanup);
        let r2 = classify_restart_action(&input(
            Some(SavedExecutionState::Flat),
            HoldingsSnapshot::Present {
                qty: 5,
                avg_price: None,
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(
            r2,
            RestartAction::OrphanHoldingDetected {
                qty: 5,
                avg_price: None
            }
        );
    }

    // ── LookupFailed: 항상 fail-safe manual ───────────────────────────────

    #[test]
    fn lookup_failed_always_forces_manual_fail_safe_regardless_of_saved_state() {
        let cases = [
            None,
            Some(entry_pending_state(10)),
            Some(open_state(10)),
            Some(exit_pending_state(10)),
            Some(manual_state(10)),
        ];
        for saved in cases {
            let r = classify_restart_action(&input(
                saved,
                HoldingsSnapshot::LookupFailed {
                    reason: "api_timeout".to_string(),
                },
                ExecutionQuerySummary::NotApplicable,
            ));
            match r {
                RestartAction::ManualFailSafe { reason } => assert_eq!(reason, "api_timeout"),
                other => panic!("expected ManualFailSafe, got {:?}", other),
            }
        }
    }

    // ── ManualIntervention saved: 항상 복원 (잔고 힌트 무시) ──────────────

    #[test]
    fn manual_saved_always_restored_even_when_balance_zero() {
        let r = classify_restart_action(&input(
            Some(manual_state(5)),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(r, RestartAction::ManualRestore { qty: 5 });
    }

    #[test]
    fn manual_saved_takes_priority_over_holdings_present() {
        let r = classify_restart_action(&input(
            Some(manual_state(5)),
            HoldingsSnapshot::Present {
                qty: 10,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        // Manual 은 저장된 quantity 를 유지 — 잔고가 더 많아도 manual 결정 보존.
        assert_eq!(r, RestartAction::ManualRestore { qty: 5 });
    }

    // ── EntryPending 경로 ──────────────────────────────────────────────────

    #[test]
    fn entry_pending_with_holdings_returns_manual_restore() {
        let r = classify_restart_action(&input(
            Some(entry_pending_state(10)),
            HoldingsSnapshot::Present {
                qty: 10,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::Filled {
                qty: 10,
                avg_price: 10_000,
            },
        ));
        assert_eq!(r, RestartAction::EntryPendingManualRestore { qty: 10 });
    }

    #[test]
    fn entry_pending_no_holdings_auto_clear_when_query_ok() {
        let r = classify_restart_action(&input(
            Some(entry_pending_state(10)),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::Unfilled,
        ));
        assert_eq!(r, RestartAction::EntryPendingAutoClear);
    }

    #[test]
    fn entry_pending_no_holdings_but_query_failed_goes_manual() {
        let r = classify_restart_action(&input(
            Some(entry_pending_state(10)),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::QueryFailed {
                reason: "rest_500".to_string(),
            },
        ));
        match r {
            RestartAction::EntryPendingManualOnQueryFail { reason } => {
                assert_eq!(reason, "rest_500");
            }
            other => panic!("expected EntryPendingManualOnQueryFail, got {:?}", other),
        }
    }

    #[test]
    fn entry_partial_same_as_entry_pending_for_dispatch() {
        let partial = SavedExecutionState::EntryPartial {
            pending_entry_order_no: "0000000002".to_string(),
            side: PositionSide::Long,
            entry_price: 10_000,
            quantity: 3,
        };
        let with_h = classify_restart_action(&input(
            Some(partial.clone()),
            HoldingsSnapshot::Present {
                qty: 3,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::Filled {
                qty: 3,
                avg_price: 10_000,
            },
        ));
        assert_eq!(with_h, RestartAction::EntryPendingManualRestore { qty: 3 });
        let no_h = classify_restart_action(&input(
            Some(partial),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::Unfilled,
        ));
        assert_eq!(no_h, RestartAction::EntryPendingAutoClear);
    }

    // ── ExitPending / ExitPartial 경로 ─────────────────────────────────────

    #[test]
    fn exit_pending_with_holdings_returns_manual_restore() {
        let r = classify_restart_action(&input(
            Some(exit_pending_state(10)),
            HoldingsSnapshot::Present {
                qty: 10,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(r, RestartAction::ExitPendingManualRestore { qty: 10 });
    }

    #[test]
    fn exit_pending_no_holdings_returns_auto_flat() {
        let r = classify_restart_action(&input(
            Some(exit_pending_state(10)),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(r, RestartAction::ExitPendingAutoFlat);
    }

    #[test]
    fn exit_partial_same_as_exit_pending_for_dispatch() {
        let partial = SavedExecutionState::ExitPartial {
            side: PositionSide::Long,
            entry_price: 10_000,
            stop_loss: 9_900,
            take_profit: 10_250,
            quantity: 7,
        };
        let with_h = classify_restart_action(&input(
            Some(partial.clone()),
            HoldingsSnapshot::Present {
                qty: 4,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(with_h, RestartAction::ExitPendingManualRestore { qty: 4 });
        let no_h = classify_restart_action(&input(
            Some(partial),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(no_h, RestartAction::ExitPendingAutoFlat);
    }

    // ── Open 경로 ──────────────────────────────────────────────────────────

    #[test]
    fn open_with_holdings_returns_position_restore_with_bracket() {
        let r = classify_restart_action(&input(
            Some(open_state(10)),
            HoldingsSnapshot::Present {
                qty: 10,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(
            r,
            RestartAction::OpenPositionRestore {
                qty: 10,
                side: PositionSide::Long,
                entry_price: 10_000,
                stop_loss: 9_900,
                take_profit: 10_250,
            }
        );
    }

    #[test]
    fn open_with_partial_holdings_uses_actual_balance_quantity() {
        // 2026-04-19 PR #1 기준선: build_restart_restored_position 이 holdings_qty 사용.
        // 저장 10 주여도 실제 잔고 3 주면 3 주로 복원.
        let r = classify_restart_action(&input(
            Some(open_state(10)),
            HoldingsSnapshot::Present {
                qty: 3,
                avg_price: Some(10_000),
            },
            ExecutionQuerySummary::NotApplicable,
        ));
        match r {
            RestartAction::OpenPositionRestore { qty, .. } => assert_eq!(qty, 3),
            other => panic!("expected OpenPositionRestore{{qty=3}}, got {:?}", other),
        }
    }

    #[test]
    fn open_no_holdings_returns_phantom_cleanup() {
        let r = classify_restart_action(&input(
            Some(open_state(10)),
            HoldingsSnapshot::None,
            ExecutionQuerySummary::NotApplicable,
        ));
        assert_eq!(r, RestartAction::OpenPhantomCleanup);
    }

    // ── label 메서드: 모든 variant 이 고유 라벨을 반환한다 ──────────────────

    // ── journal_phase: taxonomy 일치 + 허용 집합 ───────────────────────────

    #[test]
    fn journal_phases_match_taxonomy_set() {
        // execution-journal-taxonomy.md §1.3 재시작 복구 phase 집합.
        let allowed: std::collections::HashSet<&'static str> = [
            "orphan_restart_cleanup",
            "orphan_restart_holding_detected",
            "restart_balance_lookup_failed_manual",
            "manual_restart_restore",
            "entry_restart_auto_clear",
            "entry_restart_manual_intervention",
            "exit_restart_auto_flat",
            "exit_restart_manual_intervention",
            "open_restart_restore",
            "open_restart_phantom_cleanup",
        ]
        .into_iter()
        .collect();

        let actions = [
            RestartAction::OrphanCleanup,
            RestartAction::OrphanHoldingDetected {
                qty: 1,
                avg_price: None,
            },
            RestartAction::ManualFailSafe {
                reason: String::new(),
            },
            RestartAction::ManualRestore { qty: 1 },
            RestartAction::EntryPendingManualRestore { qty: 1 },
            RestartAction::EntryPendingAutoClear,
            RestartAction::EntryPendingManualOnQueryFail {
                reason: String::new(),
            },
            RestartAction::ExitPendingManualRestore { qty: 1 },
            RestartAction::ExitPendingAutoFlat,
            RestartAction::OpenPositionRestore {
                qty: 1,
                side: PositionSide::Long,
                entry_price: 0,
                stop_loss: 0,
                take_profit: 0,
            },
            RestartAction::OpenPhantomCleanup,
        ];
        for a in &actions {
            let phase = a.journal_phase();
            assert!(
                allowed.contains(phase),
                "action {:?} 의 journal_phase {:?} 가 taxonomy 집합 밖",
                a,
                phase
            );
        }
    }

    #[test]
    fn entry_pending_manual_variants_share_one_taxonomy_phase() {
        // 의도: query 실패와 holdings 잔존은 다른 action 이지만 운영 쿼리 관점에서
        // 동일 "entry 재시작 manual" 이므로 같은 phase 로 기록한다.
        let with_holdings = RestartAction::EntryPendingManualRestore { qty: 10 };
        let on_query_fail = RestartAction::EntryPendingManualOnQueryFail {
            reason: "rest_timeout".into(),
        };
        assert_eq!(with_holdings.journal_phase(), on_query_fail.journal_phase());
        assert_eq!(
            with_holdings.journal_phase(),
            "entry_restart_manual_intervention"
        );
    }

    #[test]
    fn labels_are_unique_per_variant() {
        let actions = [
            RestartAction::OrphanCleanup,
            RestartAction::OrphanHoldingDetected {
                qty: 1,
                avg_price: None,
            },
            RestartAction::ManualFailSafe {
                reason: String::new(),
            },
            RestartAction::ManualRestore { qty: 1 },
            RestartAction::EntryPendingManualRestore { qty: 1 },
            RestartAction::EntryPendingAutoClear,
            RestartAction::EntryPendingManualOnQueryFail {
                reason: String::new(),
            },
            RestartAction::ExitPendingManualRestore { qty: 1 },
            RestartAction::ExitPendingAutoFlat,
            RestartAction::OpenPositionRestore {
                qty: 1,
                side: PositionSide::Long,
                entry_price: 0,
                stop_loss: 0,
                take_profit: 0,
            },
            RestartAction::OpenPhantomCleanup,
        ];
        let mut labels: Vec<&'static str> = actions.iter().map(|a| a.label()).collect();
        let total = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), total, "label 중복 있음");
    }

    // ── PR #3.f: validate_restart_manual_metadata ──────────────────────────

    #[test]
    fn validator_rejects_when_common_keys_missing() {
        let metadata = serde_json::json!({});
        let missing =
            validate_restart_manual_metadata("entry_restart_manual_intervention", &metadata);
        assert!(missing.contains(&"prior_execution_state"));
        assert!(missing.contains(&"manual_category"));
    }

    #[test]
    fn validator_accepts_entry_manual_complete() {
        let metadata = serde_json::json!({
            "prior_execution_state": "entry_pending",
            "manual_category": "cancel_failed",
            "pending_entry_order_no": "0000000123",
        });
        let missing =
            validate_restart_manual_metadata("entry_restart_manual_intervention", &metadata);
        assert!(missing.is_empty(), "unexpected missing keys: {:?}", missing);
    }

    #[test]
    fn validator_rejects_entry_manual_missing_pending_entry_order_no() {
        let metadata = serde_json::json!({
            "prior_execution_state": "entry_pending",
            "manual_category": "cancel_failed",
        });
        let missing =
            validate_restart_manual_metadata("entry_restart_manual_intervention", &metadata);
        assert_eq!(missing, vec!["pending_entry_order_no"]);
    }

    #[test]
    fn validator_accepts_exit_manual_complete() {
        let metadata = serde_json::json!({
            "prior_execution_state": "exit_pending",
            "manual_category": "exit_pending_holdings_remaining",
            "last_exit_order_no": "0000000999",
            "last_exit_reason": "stop_loss",
        });
        let missing =
            validate_restart_manual_metadata("exit_restart_manual_intervention", &metadata);
        assert!(missing.is_empty(), "unexpected missing keys: {:?}", missing);
    }

    #[test]
    fn validator_rejects_exit_manual_missing_exit_metadata() {
        let metadata = serde_json::json!({
            "prior_execution_state": "exit_pending",
            "manual_category": "exit_pending_balance_lookup_failed",
        });
        let missing =
            validate_restart_manual_metadata("exit_restart_manual_intervention", &metadata);
        assert!(missing.contains(&"last_exit_order_no"));
        assert!(missing.contains(&"last_exit_reason"));
    }

    #[test]
    fn validator_accepts_orphan_holding_detected_complete() {
        let metadata = serde_json::json!({
            "prior_execution_state": "absent",
            "manual_category": "orphan_holding_detected",
            "quantity": 10,
        });
        let missing =
            validate_restart_manual_metadata("orphan_restart_holding_detected", &metadata);
        assert!(missing.is_empty(), "unexpected missing keys: {:?}", missing);
    }

    #[test]
    fn validator_rejects_orphan_holding_detected_missing_quantity() {
        let metadata = serde_json::json!({
            "prior_execution_state": "absent",
            "manual_category": "orphan_holding_detected",
        });
        let missing =
            validate_restart_manual_metadata("orphan_restart_holding_detected", &metadata);
        assert_eq!(missing, vec!["quantity"]);
    }

    #[test]
    fn validator_accepts_manual_restore_complete() {
        let metadata = serde_json::json!({
            "prior_execution_state": "manual_intervention",
            "manual_category": "watchdog_restart_preserved",
            "stop_loss": 9_900,
            "take_profit": 10_250,
        });
        let missing = validate_restart_manual_metadata("manual_restart_restore", &metadata);
        assert!(missing.is_empty(), "unexpected missing keys: {:?}", missing);
    }

    #[test]
    fn validator_rejects_manual_restore_missing_bracket() {
        let metadata = serde_json::json!({
            "prior_execution_state": "manual_intervention",
            "manual_category": "watchdog_restart_preserved",
        });
        let missing = validate_restart_manual_metadata("manual_restart_restore", &metadata);
        assert!(missing.contains(&"stop_loss"));
        assert!(missing.contains(&"take_profit"));
    }

    #[test]
    fn validator_accepts_balance_lookup_failed_with_only_common_keys() {
        // 이 phase 는 DB 메타가 없어 phase-specific 키는 요구하지 않는다.
        let metadata = serde_json::json!({
            "prior_execution_state": "absent",
            "manual_category": "balance_lookup_failed",
        });
        let missing =
            validate_restart_manual_metadata("restart_balance_lookup_failed_manual", &metadata);
        assert!(missing.is_empty(), "unexpected missing keys: {:?}", missing);
    }

    #[test]
    fn validator_unknown_phase_checks_only_common_keys() {
        // 알려지지 않은 phase — 공통 키만 체크해서 최소 일관성 유지.
        let metadata = serde_json::json!({
            "prior_execution_state": "entry_pending",
            "manual_category": "some_future_category",
        });
        let missing = validate_restart_manual_metadata("entry_restart_future_phase", &metadata);
        assert!(missing.is_empty());
    }

    // ── requires_manual_intervention: 진행 중 분기 ─────────────────────────

    #[test]
    fn requires_manual_intervention_matches_expected_set() {
        let manual_actions = [
            RestartAction::OrphanHoldingDetected {
                qty: 1,
                avg_price: None,
            },
            RestartAction::ManualFailSafe {
                reason: String::new(),
            },
            RestartAction::ManualRestore { qty: 1 },
            RestartAction::EntryPendingManualRestore { qty: 1 },
            RestartAction::EntryPendingManualOnQueryFail {
                reason: String::new(),
            },
            RestartAction::ExitPendingManualRestore { qty: 1 },
        ];
        for a in &manual_actions {
            assert!(a.requires_manual_intervention(), "{:?}", a);
        }
        let auto_actions = [
            RestartAction::OrphanCleanup,
            RestartAction::EntryPendingAutoClear,
            RestartAction::ExitPendingAutoFlat,
            RestartAction::OpenPositionRestore {
                qty: 1,
                side: PositionSide::Long,
                entry_price: 0,
                stop_loss: 0,
                take_profit: 0,
            },
            RestartAction::OpenPhantomCleanup,
        ];
        for a in &auto_actions {
            assert!(!a.requires_manual_intervention(), "{:?}", a);
        }
    }
}
