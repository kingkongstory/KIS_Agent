//! 2026-04-20 PR #4: execution actor 최소 골격.
//!
//! # 문서 근거
//!
//! - `docs/monitoring/2026-04-18-execution-timing-implementation-plan.md` §Phase 2
//!   (목표 상태 모델) 및 "종목별 Execution Actor 도입"
//! - `docs/monitoring/2026-04-19-execution-followup-plan.md` §5 "execution actor
//!   도입 준비 단계" — 이번 PR 의 정확한 범위.
//!
//! # 이번 단계의 목표
//!
//! 현재 `live_runner.rs` (10,000+ 줄) 에는 신호 탐색, 주문 제출, 체결 확인, 상태
//! 전이가 모두 섞여 있다. 이를 한 번에 갈아엎는 대신, 실행 계층 재설계의 **state
//! machine 부분만** 를 pure function 으로 추출한다.
//!
//! - ✅ 이벤트/명령 enum 과 상태 전이 pure fn 스켈레톤
//! - ✅ 전이 규칙 단위 테스트 (accept / reject 분리)
//! - ✅ execution_journal phase 이름과 대응하는 label helper
//! - ❌ 실제 async actor task (후속 PR)
//! - ❌ `live_runner` 호출 경로 교체 (후속 PR — 이번엔 코드 경로 변경 없음)
//! - ❌ 전략 계산 (signal 생성, gate 계산) — 이 모듈에 들어오면 안 됨
//!
//! # 책임 경계
//!
//! actor 가 **책임지는 것**:
//! - 주문 제출/취소 결정
//! - 체결 확인 결정
//! - 상태 전이
//! - timeout 관리
//! - 잔고 reconcile 지시
//!
//! actor 가 **책임지지 않는 것** (계획서 §5 구현 원칙):
//! - signal 생성 (FVG 탐지, OR 계산)
//! - gate 계산 (NAV, regime, spread, subscription health)
//! - 전략 파라미터 (SL/TP 비율, entry_cutoff)
//! - 외부 I/O 자체 (actor 는 **의도** 만 반환, 실제 I/O 는 caller 가)
//!
//! # pure fn 설계 이유
//!
//! `apply_execution_event` 는 `(ExecutionState, ExecutionEvent) → ExecutionTransition`.
//! 부작용/비동기/잠금이 전부 호출측에 남아서, actor 로직 자체는 단위 테스트로
//! 완전 고정 가능. 회귀가 발생해도 테스트에서 먼저 터진다.

use std::time::Duration;

use crate::strategy::live_runner::ExecutionState;

/// 실행 액터가 소비하는 이벤트. 상태 전이의 유일한 입력.
///
/// 타이밍 계획서 §"종목별 Execution Actor 도입" 의 이벤트 목록과 1:1 대응.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionEvent {
    /// Signal Engine 이 검증된 신호를 전달. Flat → SignalArmed.
    SignalTriggered { signal_id: String },
    /// 진입 주문이 브로커에 접수됨 (REST 응답 + 주문번호 수령).
    /// SignalArmed → EntryPending.
    EntryOrderAccepted { broker_order_id: String },
    /// WS 체결통보 또는 REST 체결조회 중간 결과.
    /// EntryPending → (EntryPartial | Open), ExitPending → (ExitPartial | verify).
    ExecutionNoticeReceived {
        broker_order_id: String,
        /// 이 시점 누적 체결 수량.
        filled_qty: u64,
        /// 주문 목표 수량.
        target_qty: u64,
    },
    /// REST 체결조회 완료 — 최종 체결 수량 확정.
    ExecutionQueryResolved {
        broker_order_id: String,
        final_qty: u64,
        target_qty: u64,
    },
    /// 잔고 조회 결과 반영. Exit verify 경로에서 최종 판정 근거.
    BalanceReconciled { holdings_qty: u64 },
    /// 청산 요청 (SL/TP 도달, 시간스탑, 수동 트리거). Open → ExitPending.
    ExitRequested { reason: String },
    /// 청산 주문이 브로커에 접수됨. ExitPending 유지 (no-op transition).
    ExitOrderAccepted { broker_order_id: String },
    /// timeout — 의미는 `TimeoutPhase` 에 따라 다름.
    TimeoutExpired { phase: TimeoutPhase },
    /// 어떤 상태에서든 manual 전환 — reconcile mismatch, cancel fail 등.
    /// 단, 이미 ManualIntervention 이면 no-op.
    ManualInterventionRequired { reason: String },
}

/// timeout 이벤트의 세부 분류. 같은 TimeoutExpired 라도 phase 에 따라 전이/명령이 다르다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeoutPhase {
    /// 진입 주문 응답 대기 timeout (SignalArmed).
    EntrySubmit,
    /// 진입 체결 대기 timeout (EntryPending).
    EntryFill,
    /// 청산 체결/보유 확인 timeout (ExitPending, ExitPartial).
    ExitFill,
}

/// Actor 가 caller 에게 지시하는 side-effect 의도.
///
/// Actor 자신은 I/O 를 수행하지 않는다. 이 명령을 반환하면 caller (현 단계에서는
/// live_runner, 후속 단계에서는 전용 actor task) 가 실제 REST/WS/DB 호출을 수행.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionCommand {
    /// 진입 주문 제출 (지정가 / 시장성 지정가 정책은 caller 결정).
    SubmitEntryOrder { signal_id: String },
    /// 청산 주문 제출 (TP 지정가 / SL 시장가는 caller 결정).
    SubmitExitOrder { reason: String },
    /// 미체결 주문 취소.
    CancelOrder { broker_order_id: String },
    /// REST 체결조회.
    QueryExecution { broker_order_id: String },
    /// 잔고 조회 + holdings 갱신.
    ReconcileBalance,
    /// manual 전환 — event_log + execution_journal + state + lock.
    EnterManualIntervention { reason: String },
    /// active_positions DB 갱신 (state 변화 영속화).
    PersistState,
}

/// 상태 전이 결과 — 새 상태 + 수행할 명령 list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionTransition {
    pub new_state: ExecutionState,
    pub commands: Vec<ExecutionCommand>,
}

impl ExecutionTransition {
    fn same_state(state: ExecutionState) -> Self {
        Self {
            new_state: state,
            commands: Vec::new(),
        }
    }

    fn to(new_state: ExecutionState) -> Self {
        Self {
            new_state,
            commands: Vec::new(),
        }
    }

    fn to_with(new_state: ExecutionState, commands: Vec<ExecutionCommand>) -> Self {
        Self {
            new_state,
            commands,
        }
    }
}

/// 현재 상태에서 허용되지 않는 이벤트.
///
/// actor 는 이를 받으면 로그만 남기고 상태는 바꾸지 않는다. 진짜 이상 상황이면
/// 상위 경로에서 `ManualInterventionRequired` 이벤트로 명시적으로 전환.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTransition {
    pub state: ExecutionState,
    pub event_label: &'static str,
}

/// 이벤트 → 상태 전이 pure function.
///
/// 규칙:
/// - `ManualIntervention` 상태에서는 `ManualInterventionRequired` 를 포함한 모든 이벤트가
///   no-op (자동 탈출 금지 — 운영자만 해제 가능).
/// - `Degraded` 상태에서는 신규 진입 (`SignalTriggered`) 만 거부되고, 기존 포지션 관리
///   경로 (청산, 체결 확인 등) 는 허용. Degraded 는 "신규 차단" 만 의미하기 때문.
/// - 알려지지 않은 조합은 `InvalidTransition` — actor 가 상위에 보고해 root cause 를
///   추적.
pub fn apply_execution_event(
    state: ExecutionState,
    event: ExecutionEvent,
) -> Result<ExecutionTransition, InvalidTransition> {
    // ManualIntervention 은 모든 이벤트 흡수 (자동 탈출 금지).
    if state == ExecutionState::ManualIntervention {
        return Ok(ExecutionTransition::same_state(state));
    }

    match (state, &event) {
        // ── Flat ────────────────────────────────────────────────────────────
        (ExecutionState::Flat, ExecutionEvent::SignalTriggered { .. }) => {
            Ok(ExecutionTransition::to(ExecutionState::SignalArmed))
        }
        (ExecutionState::Flat, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── SignalArmed ─────────────────────────────────────────────────────
        (ExecutionState::SignalArmed, ExecutionEvent::EntryOrderAccepted { .. }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::EntryPending,
                vec![ExecutionCommand::PersistState],
            ))
        }
        (
            ExecutionState::SignalArmed,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::EntrySubmit,
            },
        ) => Ok(ExecutionTransition::to(ExecutionState::Flat)),
        (ExecutionState::SignalArmed, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── EntryPending ────────────────────────────────────────────────────
        (
            ExecutionState::EntryPending,
            ExecutionEvent::ExecutionNoticeReceived {
                filled_qty,
                target_qty,
                ..
            },
        ) => {
            if *filled_qty == 0 {
                Ok(ExecutionTransition::same_state(state))
            } else if filled_qty >= target_qty {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Open,
                    vec![ExecutionCommand::PersistState],
                ))
            } else {
                Ok(ExecutionTransition::to(ExecutionState::EntryPartial))
            }
        }
        (
            ExecutionState::EntryPending,
            ExecutionEvent::ExecutionQueryResolved {
                final_qty,
                target_qty,
                ..
            },
        ) => {
            if final_qty >= target_qty {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Open,
                    vec![ExecutionCommand::PersistState],
                ))
            } else if *final_qty == 0 {
                // 최종 0 체결 — 주문 취소/거절 → Flat.
                Ok(ExecutionTransition::to(ExecutionState::Flat))
            } else {
                Ok(ExecutionTransition::to(ExecutionState::EntryPartial))
            }
        }
        (ExecutionState::EntryPending, ExecutionEvent::BalanceReconciled { holdings_qty }) => {
            if *holdings_qty > 0 {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Open,
                    vec![ExecutionCommand::PersistState],
                ))
            } else {
                Ok(ExecutionTransition::same_state(state))
            }
        }
        (
            ExecutionState::EntryPending,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::EntryFill,
            },
        ) => {
            // 주문 미체결 → 취소 명령 + 잔고 재확인. 상태는 Flat 으로 잠정 이동
            // 하되, 이후 BalanceReconciled(>0) 가 오면 Open 으로 올라갈 수 있다.
            Ok(ExecutionTransition::to_with(
                ExecutionState::Flat,
                vec![
                    ExecutionCommand::CancelOrder {
                        broker_order_id: String::new(),
                    },
                    ExecutionCommand::ReconcileBalance,
                ],
            ))
        }
        (ExecutionState::EntryPending, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── EntryPartial ────────────────────────────────────────────────────
        (
            ExecutionState::EntryPartial,
            ExecutionEvent::ExecutionNoticeReceived {
                filled_qty,
                target_qty,
                ..
            },
        ) => {
            if filled_qty >= target_qty {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Open,
                    vec![ExecutionCommand::PersistState],
                ))
            } else {
                Ok(ExecutionTransition::same_state(state))
            }
        }
        (ExecutionState::EntryPartial, ExecutionEvent::BalanceReconciled { holdings_qty }) => {
            if *holdings_qty > 0 {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Open,
                    vec![ExecutionCommand::PersistState],
                ))
            } else {
                Ok(ExecutionTransition::same_state(state))
            }
        }
        (
            ExecutionState::EntryPartial,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::EntryFill,
            },
        ) => Ok(ExecutionTransition::to_with(
            ExecutionState::ManualIntervention,
            vec![
                ExecutionCommand::EnterManualIntervention {
                    reason: "entry_partial_fill_timeout".to_string(),
                },
                ExecutionCommand::PersistState,
            ],
        )),
        (ExecutionState::EntryPartial, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── Open ────────────────────────────────────────────────────────────
        (ExecutionState::Open, ExecutionEvent::ExitRequested { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ExitPending,
                vec![ExecutionCommand::SubmitExitOrder {
                    reason: reason.clone(),
                }],
            ))
        }
        (ExecutionState::Open, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── ExitPending ─────────────────────────────────────────────────────
        (ExecutionState::ExitPending, ExecutionEvent::ExitOrderAccepted { .. }) => {
            Ok(ExecutionTransition::same_state(state))
        }
        (
            ExecutionState::ExitPending,
            ExecutionEvent::ExecutionNoticeReceived {
                filled_qty,
                target_qty,
                ..
            },
        ) => {
            if filled_qty >= target_qty {
                // 체결은 확인됐지만 holdings 0 은 별도 이벤트로 확정 (불변식 2).
                Ok(ExecutionTransition::to_with(
                    ExecutionState::ExitPending,
                    vec![ExecutionCommand::ReconcileBalance],
                ))
            } else if *filled_qty > 0 {
                Ok(ExecutionTransition::to(ExecutionState::ExitPartial))
            } else {
                Ok(ExecutionTransition::same_state(state))
            }
        }
        (ExecutionState::ExitPending, ExecutionEvent::BalanceReconciled { holdings_qty }) => {
            if *holdings_qty == 0 {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Flat,
                    vec![ExecutionCommand::PersistState],
                ))
            } else {
                Ok(ExecutionTransition::to(ExecutionState::ExitPartial))
            }
        }
        (
            ExecutionState::ExitPending,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::ExitFill,
            },
        ) => Ok(ExecutionTransition::to_with(
            ExecutionState::ManualIntervention,
            vec![
                ExecutionCommand::EnterManualIntervention {
                    reason: "exit_fill_timeout".to_string(),
                },
                ExecutionCommand::PersistState,
            ],
        )),
        (ExecutionState::ExitPending, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── ExitPartial ─────────────────────────────────────────────────────
        (ExecutionState::ExitPartial, ExecutionEvent::BalanceReconciled { holdings_qty }) => {
            if *holdings_qty == 0 {
                Ok(ExecutionTransition::to_with(
                    ExecutionState::Flat,
                    vec![ExecutionCommand::PersistState],
                ))
            } else {
                Ok(ExecutionTransition::same_state(state))
            }
        }
        (
            ExecutionState::ExitPartial,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::ExitFill,
            },
        ) => Ok(ExecutionTransition::to_with(
            ExecutionState::ManualIntervention,
            vec![
                ExecutionCommand::EnterManualIntervention {
                    reason: "exit_partial_fill_timeout".to_string(),
                },
                ExecutionCommand::PersistState,
            ],
        )),
        (ExecutionState::ExitPartial, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── Degraded ────────────────────────────────────────────────────────
        (ExecutionState::Degraded, ExecutionEvent::SignalTriggered { .. }) => {
            // 신규 진입 차단 (§2 불변식 "실시간 구독 health 비정상이면 신규 진입 금지").
            // 상태 유지 — caller 가 signal 을 drop.
            Ok(ExecutionTransition::same_state(state))
        }
        (ExecutionState::Degraded, ExecutionEvent::ManualInterventionRequired { reason }) => {
            Ok(ExecutionTransition::to_with(
                ExecutionState::ManualIntervention,
                vec![
                    ExecutionCommand::EnterManualIntervention {
                        reason: reason.clone(),
                    },
                    ExecutionCommand::PersistState,
                ],
            ))
        }

        // ── 기타: invalid transition ────────────────────────────────────────
        _ => Err(InvalidTransition {
            state,
            event_label: event_label(&event),
        }),
    }
}

/// 2026-04-20 PR #5.a: live_runner 의 실 전이 pair 가 actor rule 로 설명 가능한지.
///
/// `transition_execution_state` 가 shadow validation 으로 호출한다. 이 함수는
/// "actor 규칙 하에서 어떤 이벤트라도 이 전이를 만들 수 있는가" 를 pure 하게 판정.
/// false 이면 live_runner 의 전이 경로가 actor 규칙에서 벗어났다는 신호 — runtime
/// 블록은 하지 않고 event_log 에 `actor_rule_mismatch` 로 기록해 사후 추적.
///
/// 일부 전이는 actor 책임 밖 (외부 gate 상태 변화) 이라 명시적으로 허용:
///
/// - `Degraded ↔ {Open, Flat, SignalArmed}`: preflight health 변화에 따른 외부 전이.
///   `transition_to_degraded` / `clear_degraded_if_healthy` 가 strategy/data 계층
///   판단으로 수행하므로 actor event 로 표현 안 됨.
/// - `Flat → EntryPartial`: timeout/cancel 직후 뒤늦은 체결을 balance 로 확인한
///   복구 전이. 주문 lifecycle 은 이미 취소 경로에 들어갔지만 실제 보유 수량이
///   생긴 경우이므로 유령 포지션 방지를 위해 정상 복구 경로로 인정한다.
pub fn is_actor_allowed_transition(prev: ExecutionState, new: ExecutionState) -> bool {
    // Idempotent — transition_execution_state 는 prev==new 일 때 early return 하지만
    // 혹시 호출되면 safe.
    if prev == new {
        return true;
    }

    // ── Degraded 는 외부 gate 경로. actor event 집합 밖이므로 whitelist. ──
    if prev == ExecutionState::Degraded
        && matches!(
            new,
            ExecutionState::Open | ExecutionState::Flat | ExecutionState::SignalArmed
        )
    {
        return true;
    }
    if matches!(
        prev,
        ExecutionState::Flat | ExecutionState::SignalArmed | ExecutionState::Open
    ) && new == ExecutionState::Degraded
    {
        return true;
    }
    if prev == ExecutionState::Flat && new == ExecutionState::EntryPartial {
        return true;
    }

    // ── actor event 샘플 enumerate — filled_qty 가 결과를 바꾸므로 partial/full/zero
    //    세 샘플 모두 포함. 하나라도 (prev, event) → new 면 허용. ──
    let samples: [ExecutionEvent; 15] = [
        ExecutionEvent::SignalTriggered {
            signal_id: String::new(),
        },
        ExecutionEvent::EntryOrderAccepted {
            broker_order_id: String::new(),
        },
        ExecutionEvent::ExecutionNoticeReceived {
            broker_order_id: String::new(),
            filled_qty: 10,
            target_qty: 10,
        },
        ExecutionEvent::ExecutionNoticeReceived {
            broker_order_id: String::new(),
            filled_qty: 3,
            target_qty: 10,
        },
        ExecutionEvent::ExecutionNoticeReceived {
            broker_order_id: String::new(),
            filled_qty: 0,
            target_qty: 10,
        },
        ExecutionEvent::ExecutionQueryResolved {
            broker_order_id: String::new(),
            final_qty: 10,
            target_qty: 10,
        },
        ExecutionEvent::ExecutionQueryResolved {
            broker_order_id: String::new(),
            final_qty: 0,
            target_qty: 10,
        },
        ExecutionEvent::BalanceReconciled { holdings_qty: 5 },
        ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
        ExecutionEvent::ExitRequested {
            reason: String::new(),
        },
        ExecutionEvent::ExitOrderAccepted {
            broker_order_id: String::new(),
        },
        ExecutionEvent::TimeoutExpired {
            phase: TimeoutPhase::EntrySubmit,
        },
        ExecutionEvent::TimeoutExpired {
            phase: TimeoutPhase::EntryFill,
        },
        ExecutionEvent::TimeoutExpired {
            phase: TimeoutPhase::ExitFill,
        },
        ExecutionEvent::ManualInterventionRequired {
            reason: String::new(),
        },
    ];

    samples.iter().any(|e| {
        apply_execution_event(prev, e.clone())
            .ok()
            .map(|t| t.new_state == new)
            .unwrap_or(false)
    })
}

// ── 2026-04-20 #5.d-design → 2026-04-21 #5.d-wire: Timeout 정책 ───────────
//
// 처음에는 pure artifact 로만 추가되었고, #5.d-wire 에서 `live_runner.rs` 의
// armed/entry/exit timeout 경로가 이 타입과 `next_timeout_for` 를 실제로 사용한다.
//
// 현재도 "완전한 async actor queue" 는 아니다. 다만 timeout 분해(EntrySubmit /
// EntryFill / ExitFill) 와 `TimeoutExpired` state rule 은 더 이상 문서/테스트 전용이
// 아니라 실 runtime 경로에 연결되어 있다.

/// 실행 phase 별 기본 timeout. 후속 `actor task` 가 state 진입 시 조회해서
/// `tokio::time::sleep(duration)` 을 걸고, 만료 시 `TimeoutExpired { phase }`
/// 이벤트를 self-dispatch 한다.
///
/// 각 phase 의 의미는 `TimeoutPhase` 주석 참고. 값은 실행 계획서
/// `2026-04-18-execution-timing-implementation-plan.md` §"`30초`에 대한 명시적
/// 판단" 및 §"`1-1. execute_entry()` timeout 분해" 를 반영.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutPolicy {
    /// SignalArmed 상태에서 진입 주문 REST 응답을 기다리는 최대 시간.
    /// 응답이 이 시간 안에 오지 않으면 `TimeoutExpired { EntrySubmit }` 이벤트 →
    /// actor rule: SignalArmed → Flat.
    pub entry_submit: Duration,
    /// EntryPending / EntryPartial 상태에서 체결 확인을 기다리는 최대 시간.
    /// 만료 시 `TimeoutExpired { EntryFill }` →
    /// actor rule: EntryPending → Flat (+ cancel), EntryPartial → ManualIntervention.
    pub entry_fill: Duration,
    /// ExitPending / ExitPartial 상태에서 청산 체결/잔고 확인을 기다리는 최대 시간.
    /// 만료 시 `TimeoutExpired { ExitFill }` →
    /// actor rule: 둘 다 ManualIntervention (불변식 5 — 상태 불명은 자동 복구 금지).
    pub exit_fill: Duration,
}

impl TimeoutPolicy {
    /// paper 모드 기본값 — 모의투자/테스트. entry_fill 은 기존 `execute_entry` 30초
    /// 루프와 호환되도록 넉넉히 둔다 (실 wire-up 시 호출부가 policy 를 override
    /// 가능하도록 설계).
    pub const fn paper_defaults() -> Self {
        Self {
            entry_submit: Duration::from_millis(2_000),
            entry_fill: Duration::from_millis(30_000),
            exit_fill: Duration::from_millis(5_000),
        }
    }

    /// real 모드 기본값 — 실전. 계획서 §"`1-1` timeout 분해" 권장값:
    /// submit 1~2s, fill 2~3s, exit verify 3~5s.
    pub const fn real_defaults() -> Self {
        Self {
            entry_submit: Duration::from_millis(2_000),
            entry_fill: Duration::from_millis(3_000),
            exit_fill: Duration::from_millis(5_000),
        }
    }
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self::paper_defaults()
    }
}

/// State 진입 시 actor 가 설정할 timeout 을 결정하는 pure function.
///
/// 반환:
/// - `None`: 이 state 에서 자발적 timeout 없음 (Flat, Open, ManualIntervention, Degraded).
///   Open 이 `None` 인 이유는 "체결 완료된 포지션" 이라 SL/TP/TimeStop 판단은 strategy
///   계층이 tick 기반으로 수행하기 때문 (actor 가 관여 안 함).
/// - `Some((phase, duration))`: actor task 가 `tokio::time::sleep(duration)` 후
///   `TimeoutExpired { phase }` 이벤트를 self-dispatch 해야 하는 경우.
///
pub fn next_timeout_for(
    state: ExecutionState,
    policy: &TimeoutPolicy,
) -> Option<(TimeoutPhase, Duration)> {
    match state {
        ExecutionState::SignalArmed => Some((TimeoutPhase::EntrySubmit, policy.entry_submit)),
        ExecutionState::EntryPending | ExecutionState::EntryPartial => {
            Some((TimeoutPhase::EntryFill, policy.entry_fill))
        }
        ExecutionState::ExitPending | ExecutionState::ExitPartial => {
            Some((TimeoutPhase::ExitFill, policy.exit_fill))
        }
        ExecutionState::Flat
        | ExecutionState::Open
        | ExecutionState::ManualIntervention
        | ExecutionState::Degraded => None,
    }
}

/// 명령의 카테고리 라벨. 로그/journal 에 찍을 short identifier.
pub fn command_label(cmd: &ExecutionCommand) -> &'static str {
    match cmd {
        ExecutionCommand::SubmitEntryOrder { .. } => "submit_entry_order",
        ExecutionCommand::SubmitExitOrder { .. } => "submit_exit_order",
        ExecutionCommand::CancelOrder { .. } => "cancel_order",
        ExecutionCommand::QueryExecution { .. } => "query_execution",
        ExecutionCommand::ReconcileBalance => "reconcile_balance",
        ExecutionCommand::EnterManualIntervention { .. } => "enter_manual_intervention",
        ExecutionCommand::PersistState => "persist_state",
    }
}

/// 이벤트의 카테고리 라벨. 로그/journal 에 찍을 short identifier.
pub fn event_label(event: &ExecutionEvent) -> &'static str {
    match event {
        ExecutionEvent::SignalTriggered { .. } => "signal_triggered",
        ExecutionEvent::EntryOrderAccepted { .. } => "entry_order_accepted",
        ExecutionEvent::ExecutionNoticeReceived { .. } => "execution_notice_received",
        ExecutionEvent::ExecutionQueryResolved { .. } => "execution_query_resolved",
        ExecutionEvent::BalanceReconciled { .. } => "balance_reconciled",
        ExecutionEvent::ExitRequested { .. } => "exit_requested",
        ExecutionEvent::ExitOrderAccepted { .. } => "exit_order_accepted",
        ExecutionEvent::TimeoutExpired { .. } => "timeout_expired",
        ExecutionEvent::ManualInterventionRequired { .. } => "manual_intervention_required",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal() -> ExecutionEvent {
        ExecutionEvent::SignalTriggered {
            signal_id: "122630:090500:10000:5".to_string(),
        }
    }

    fn entry_accepted() -> ExecutionEvent {
        ExecutionEvent::EntryOrderAccepted {
            broker_order_id: "0000000001".to_string(),
        }
    }

    fn manual_required() -> ExecutionEvent {
        ExecutionEvent::ManualInterventionRequired {
            reason: "reconcile_mismatch".to_string(),
        }
    }

    // ── Flat ──────────────────────────────────────────────────────────────

    #[test]
    fn flat_signal_triggered_goes_to_signal_armed() {
        let t = apply_execution_event(ExecutionState::Flat, signal()).unwrap();
        assert_eq!(t.new_state, ExecutionState::SignalArmed);
        assert!(t.commands.is_empty());
    }

    #[test]
    fn flat_manual_required_goes_to_manual_with_commands() {
        let t = apply_execution_event(ExecutionState::Flat, manual_required()).unwrap();
        assert_eq!(t.new_state, ExecutionState::ManualIntervention);
        assert_eq!(t.commands.len(), 2);
        assert!(matches!(
            t.commands[0],
            ExecutionCommand::EnterManualIntervention { .. }
        ));
        assert_eq!(t.commands[1], ExecutionCommand::PersistState);
    }

    #[test]
    fn flat_rejects_unrelated_events() {
        let bad = apply_execution_event(ExecutionState::Flat, entry_accepted());
        assert!(bad.is_err());
        assert_eq!(bad.unwrap_err().event_label, "entry_order_accepted");
    }

    // ── SignalArmed ───────────────────────────────────────────────────────

    #[test]
    fn signal_armed_entry_accepted_goes_to_entry_pending() {
        let t = apply_execution_event(ExecutionState::SignalArmed, entry_accepted()).unwrap();
        assert_eq!(t.new_state, ExecutionState::EntryPending);
        assert_eq!(t.commands, vec![ExecutionCommand::PersistState]);
    }

    #[test]
    fn signal_armed_entry_submit_timeout_returns_to_flat() {
        let t = apply_execution_event(
            ExecutionState::SignalArmed,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::EntrySubmit,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Flat);
    }

    #[test]
    fn signal_armed_manual_required_goes_to_manual() {
        let t = apply_execution_event(ExecutionState::SignalArmed, manual_required()).unwrap();
        assert_eq!(t.new_state, ExecutionState::ManualIntervention);
    }

    // ── EntryPending ──────────────────────────────────────────────────────

    #[test]
    fn entry_pending_partial_notice_goes_to_entry_partial() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::ExecutionNoticeReceived {
                broker_order_id: "x".into(),
                filled_qty: 3,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::EntryPartial);
    }

    #[test]
    fn entry_pending_zero_fill_notice_stays() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::ExecutionNoticeReceived {
                broker_order_id: "x".into(),
                filled_qty: 0,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::EntryPending);
    }

    #[test]
    fn entry_pending_full_notice_goes_to_open_with_persist() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::ExecutionNoticeReceived {
                broker_order_id: "x".into(),
                filled_qty: 10,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Open);
        assert_eq!(t.commands, vec![ExecutionCommand::PersistState]);
    }

    #[test]
    fn entry_pending_query_resolved_zero_returns_to_flat() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::ExecutionQueryResolved {
                broker_order_id: "x".into(),
                final_qty: 0,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Flat);
    }

    #[test]
    fn entry_pending_balance_holdings_goes_to_open() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::BalanceReconciled { holdings_qty: 5 },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Open);
    }

    #[test]
    fn entry_pending_balance_zero_stays() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::EntryPending);
    }

    #[test]
    fn entry_pending_fill_timeout_cancels_and_returns_to_flat() {
        let t = apply_execution_event(
            ExecutionState::EntryPending,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::EntryFill,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Flat);
        assert!(
            t.commands
                .iter()
                .any(|c| matches!(c, ExecutionCommand::CancelOrder { .. }))
        );
        assert!(t.commands.contains(&ExecutionCommand::ReconcileBalance));
    }

    // ── EntryPartial ──────────────────────────────────────────────────────

    #[test]
    fn entry_partial_full_notice_goes_to_open() {
        let t = apply_execution_event(
            ExecutionState::EntryPartial,
            ExecutionEvent::ExecutionNoticeReceived {
                broker_order_id: "x".into(),
                filled_qty: 10,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Open);
    }

    #[test]
    fn entry_partial_fill_timeout_goes_to_manual() {
        let t = apply_execution_event(
            ExecutionState::EntryPartial,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::EntryFill,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ManualIntervention);
    }

    // ── Open ──────────────────────────────────────────────────────────────

    #[test]
    fn open_exit_requested_goes_to_exit_pending_with_submit() {
        let t = apply_execution_event(
            ExecutionState::Open,
            ExecutionEvent::ExitRequested {
                reason: "stop_loss".into(),
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ExitPending);
        assert_eq!(t.commands.len(), 1);
        assert!(matches!(
            t.commands[0],
            ExecutionCommand::SubmitExitOrder { .. }
        ));
    }

    #[test]
    fn open_rejects_entry_accepted() {
        let bad = apply_execution_event(ExecutionState::Open, entry_accepted());
        assert!(bad.is_err());
    }

    // ── ExitPending ───────────────────────────────────────────────────────

    #[test]
    fn exit_pending_full_notice_triggers_reconcile_not_immediate_flat() {
        // 불변식 2: holdings 0 확인 전에는 Flat 금지.
        let t = apply_execution_event(
            ExecutionState::ExitPending,
            ExecutionEvent::ExecutionNoticeReceived {
                broker_order_id: "x".into(),
                filled_qty: 10,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ExitPending);
        assert_eq!(t.commands, vec![ExecutionCommand::ReconcileBalance]);
    }

    #[test]
    fn exit_pending_partial_notice_goes_to_exit_partial() {
        let t = apply_execution_event(
            ExecutionState::ExitPending,
            ExecutionEvent::ExecutionNoticeReceived {
                broker_order_id: "x".into(),
                filled_qty: 3,
                target_qty: 10,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ExitPartial);
    }

    #[test]
    fn exit_pending_balance_zero_goes_to_flat_with_persist() {
        let t = apply_execution_event(
            ExecutionState::ExitPending,
            ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Flat);
        assert_eq!(t.commands, vec![ExecutionCommand::PersistState]);
    }

    #[test]
    fn exit_pending_balance_positive_goes_to_exit_partial() {
        let t = apply_execution_event(
            ExecutionState::ExitPending,
            ExecutionEvent::BalanceReconciled { holdings_qty: 4 },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ExitPartial);
    }

    #[test]
    fn exit_pending_fill_timeout_goes_to_manual() {
        let t = apply_execution_event(
            ExecutionState::ExitPending,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::ExitFill,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ManualIntervention);
    }

    // ── ExitPartial ───────────────────────────────────────────────────────

    #[test]
    fn exit_partial_balance_zero_goes_to_flat() {
        let t = apply_execution_event(
            ExecutionState::ExitPartial,
            ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::Flat);
    }

    #[test]
    fn exit_partial_fill_timeout_goes_to_manual() {
        let t = apply_execution_event(
            ExecutionState::ExitPartial,
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::ExitFill,
            },
        )
        .unwrap();
        assert_eq!(t.new_state, ExecutionState::ManualIntervention);
    }

    // ── ManualIntervention — 자동 탈출 금지 ──────────────────────────────

    #[test]
    fn manual_intervention_absorbs_all_events() {
        let events = [
            signal(),
            entry_accepted(),
            ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
            ExecutionEvent::ExitRequested { reason: "x".into() },
            ExecutionEvent::TimeoutExpired {
                phase: TimeoutPhase::ExitFill,
            },
            manual_required(),
        ];
        for e in events {
            let t = apply_execution_event(ExecutionState::ManualIntervention, e.clone()).unwrap();
            assert_eq!(
                t.new_state,
                ExecutionState::ManualIntervention,
                "event {:?} 가 manual 을 빠져나가면 안 됨",
                e
            );
            assert!(t.commands.is_empty(), "manual 에서는 command 도 없어야 함");
        }
    }

    // ── Degraded ──────────────────────────────────────────────────────────

    #[test]
    fn degraded_rejects_new_signal_silently() {
        // Degraded 는 신규 진입만 차단 — 상태 유지, 명령 없음.
        let t = apply_execution_event(ExecutionState::Degraded, signal()).unwrap();
        assert_eq!(t.new_state, ExecutionState::Degraded);
        assert!(t.commands.is_empty());
    }

    #[test]
    fn degraded_can_escalate_to_manual() {
        let t = apply_execution_event(ExecutionState::Degraded, manual_required()).unwrap();
        assert_eq!(t.new_state, ExecutionState::ManualIntervention);
    }

    // ── is_actor_allowed_transition (PR #5.a shadow validation) ───────────

    #[test]
    fn allowed_transition_idempotent_same_state_pairs() {
        for s in [
            ExecutionState::Flat,
            ExecutionState::SignalArmed,
            ExecutionState::EntryPending,
            ExecutionState::EntryPartial,
            ExecutionState::Open,
            ExecutionState::ExitPending,
            ExecutionState::ExitPartial,
            ExecutionState::ManualIntervention,
            ExecutionState::Degraded,
        ] {
            assert!(is_actor_allowed_transition(s, s), "{:?} → {:?}", s, s);
        }
    }

    #[test]
    fn allowed_transition_covers_happy_path_entry_flow() {
        // Flat → SignalArmed → EntryPending → Open
        assert!(is_actor_allowed_transition(
            ExecutionState::Flat,
            ExecutionState::SignalArmed,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::SignalArmed,
            ExecutionState::EntryPending,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::EntryPending,
            ExecutionState::Open,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::EntryPending,
            ExecutionState::EntryPartial,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::EntryPartial,
            ExecutionState::Open,
        ));
    }

    #[test]
    fn allowed_transition_covers_happy_path_exit_flow() {
        // Open → ExitPending → ExitPartial → Flat
        // Open → ExitPending → Flat (즉시 전량 체결)
        assert!(is_actor_allowed_transition(
            ExecutionState::Open,
            ExecutionState::ExitPending,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::ExitPending,
            ExecutionState::ExitPartial,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::ExitPending,
            ExecutionState::Flat,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::ExitPartial,
            ExecutionState::Flat,
        ));
    }

    #[test]
    fn allowed_transition_covers_manual_escalation_from_all_active_states() {
        for from in [
            ExecutionState::Flat,
            ExecutionState::SignalArmed,
            ExecutionState::EntryPending,
            ExecutionState::EntryPartial,
            ExecutionState::Open,
            ExecutionState::ExitPending,
            ExecutionState::ExitPartial,
            ExecutionState::Degraded,
        ] {
            assert!(
                is_actor_allowed_transition(from, ExecutionState::ManualIntervention),
                "{:?} → Manual 은 허용되어야 함",
                from
            );
        }
    }

    #[test]
    fn allowed_transition_covers_timeout_recovery_to_flat() {
        // SignalArmed → Flat (entry_submit timeout)
        // EntryPending → Flat (entry_fill timeout + cancel)
        assert!(is_actor_allowed_transition(
            ExecutionState::SignalArmed,
            ExecutionState::Flat,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::EntryPending,
            ExecutionState::Flat,
        ));
    }

    #[test]
    fn allowed_transition_covers_cancel_fill_race_partial_recovery() {
        // timeout/cancel 직후 balance 로 부분체결을 발견하면 Flat 에서 EntryPartial 로 복구한다.
        assert!(is_actor_allowed_transition(
            ExecutionState::Flat,
            ExecutionState::EntryPartial,
        ));
    }

    #[test]
    fn allowed_transition_covers_degraded_external_gate_paths() {
        // Degraded 는 외부 gate 경로라 whitelist.
        assert!(is_actor_allowed_transition(
            ExecutionState::Flat,
            ExecutionState::Degraded,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::SignalArmed,
            ExecutionState::Degraded,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::Open,
            ExecutionState::Degraded,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::Degraded,
            ExecutionState::Flat,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::Degraded,
            ExecutionState::Open,
        ));
        assert!(is_actor_allowed_transition(
            ExecutionState::Degraded,
            ExecutionState::SignalArmed,
        ));
    }

    #[test]
    fn allowed_transition_rejects_known_invalid_pairs() {
        // ManualIntervention 은 자동 탈출 불가.
        assert!(!is_actor_allowed_transition(
            ExecutionState::ManualIntervention,
            ExecutionState::Flat,
        ));
        assert!(!is_actor_allowed_transition(
            ExecutionState::ManualIntervention,
            ExecutionState::Open,
        ));
        // Flat 에서 Open 직행은 불가 (중간 상태 필요).
        assert!(!is_actor_allowed_transition(
            ExecutionState::Flat,
            ExecutionState::Open,
        ));
        // Flat 에서 EntryPending 직행도 불가 (SignalArmed 경유 필요).
        assert!(!is_actor_allowed_transition(
            ExecutionState::Flat,
            ExecutionState::EntryPending,
        ));
        // Open 에서 EntryPending 역전도 불가.
        assert!(!is_actor_allowed_transition(
            ExecutionState::Open,
            ExecutionState::EntryPending,
        ));
    }

    // ── event_label 완전성 ────────────────────────────────────────────────

    // ── PR #5.d-design: TimeoutPolicy / next_timeout_for (wire-up 금지) ──

    #[test]
    fn timeout_policy_paper_defaults_match_legacy_behavior() {
        // paper 기본값은 기존 execute_entry 30초 루프와 호환되어야 한다.
        let p = TimeoutPolicy::paper_defaults();
        assert_eq!(p.entry_submit, Duration::from_millis(2_000));
        assert_eq!(p.entry_fill, Duration::from_millis(30_000));
        assert_eq!(p.exit_fill, Duration::from_millis(5_000));
    }

    #[test]
    fn timeout_policy_real_defaults_match_plan_recommendations() {
        // 계획서 §"1-1 timeout 분해" 권장값.
        let r = TimeoutPolicy::real_defaults();
        assert!(r.entry_submit <= Duration::from_millis(2_000));
        assert!(r.entry_fill <= Duration::from_millis(5_000));
        assert!(r.exit_fill <= Duration::from_millis(5_000));
        // real 은 paper 보다 짧거나 같아야 한다 (신호 부적합 방지).
        let p = TimeoutPolicy::paper_defaults();
        assert!(r.entry_fill <= p.entry_fill);
    }

    #[test]
    fn timeout_policy_default_is_paper_defaults() {
        assert_eq!(TimeoutPolicy::default(), TimeoutPolicy::paper_defaults());
    }

    #[test]
    fn next_timeout_signal_armed_returns_entry_submit() {
        let p = TimeoutPolicy::paper_defaults();
        let t = next_timeout_for(ExecutionState::SignalArmed, &p);
        assert_eq!(t, Some((TimeoutPhase::EntrySubmit, p.entry_submit)));
    }

    #[test]
    fn next_timeout_entry_pending_and_partial_both_use_entry_fill() {
        let p = TimeoutPolicy::paper_defaults();
        assert_eq!(
            next_timeout_for(ExecutionState::EntryPending, &p),
            Some((TimeoutPhase::EntryFill, p.entry_fill))
        );
        assert_eq!(
            next_timeout_for(ExecutionState::EntryPartial, &p),
            Some((TimeoutPhase::EntryFill, p.entry_fill))
        );
    }

    #[test]
    fn next_timeout_exit_pending_and_partial_both_use_exit_fill() {
        let p = TimeoutPolicy::paper_defaults();
        assert_eq!(
            next_timeout_for(ExecutionState::ExitPending, &p),
            Some((TimeoutPhase::ExitFill, p.exit_fill))
        );
        assert_eq!(
            next_timeout_for(ExecutionState::ExitPartial, &p),
            Some((TimeoutPhase::ExitFill, p.exit_fill))
        );
    }

    #[test]
    fn next_timeout_returns_none_for_terminal_and_quiescent_states() {
        let p = TimeoutPolicy::paper_defaults();
        for s in [
            ExecutionState::Flat,
            ExecutionState::Open,
            ExecutionState::ManualIntervention,
            ExecutionState::Degraded,
        ] {
            assert_eq!(next_timeout_for(s, &p), None, "{:?} 는 timeout 없음", s);
        }
    }

    #[test]
    fn next_timeout_uses_policy_values_not_hardcoded() {
        // 값이 policy 에서 흘러나오는지 — custom policy 로 검증.
        let custom = TimeoutPolicy {
            entry_submit: Duration::from_millis(111),
            entry_fill: Duration::from_millis(222),
            exit_fill: Duration::from_millis(333),
        };
        assert_eq!(
            next_timeout_for(ExecutionState::SignalArmed, &custom),
            Some((TimeoutPhase::EntrySubmit, Duration::from_millis(111)))
        );
        assert_eq!(
            next_timeout_for(ExecutionState::EntryPending, &custom),
            Some((TimeoutPhase::EntryFill, Duration::from_millis(222)))
        );
        assert_eq!(
            next_timeout_for(ExecutionState::ExitPending, &custom),
            Some((TimeoutPhase::ExitFill, Duration::from_millis(333)))
        );
    }

    #[test]
    fn next_timeout_covers_all_three_timeout_phases() {
        // `TimeoutPhase` 의 모든 variant 가 최소 한 번은 사용되어야 한다 —
        // 새 variant 를 추가했는데 `next_timeout_for` 매핑을 누락하는 경우 감지.
        let p = TimeoutPolicy::paper_defaults();
        let mut seen_phases: std::collections::HashSet<TimeoutPhase> =
            std::collections::HashSet::new();
        for s in [
            ExecutionState::Flat,
            ExecutionState::SignalArmed,
            ExecutionState::EntryPending,
            ExecutionState::EntryPartial,
            ExecutionState::Open,
            ExecutionState::ExitPending,
            ExecutionState::ExitPartial,
            ExecutionState::ManualIntervention,
            ExecutionState::Degraded,
        ] {
            if let Some((phase, _)) = next_timeout_for(s, &p) {
                seen_phases.insert(phase);
            }
        }
        assert_eq!(
            seen_phases.len(),
            3,
            "3 phase 모두 매핑되어야 함: seen={:?}",
            seen_phases
        );
    }

    #[test]
    fn command_label_is_stable_for_all_variants() {
        let variants = [
            (
                "submit_entry_order",
                ExecutionCommand::SubmitEntryOrder {
                    signal_id: "x".into(),
                },
            ),
            (
                "submit_exit_order",
                ExecutionCommand::SubmitExitOrder { reason: "x".into() },
            ),
            (
                "cancel_order",
                ExecutionCommand::CancelOrder {
                    broker_order_id: "x".into(),
                },
            ),
            (
                "query_execution",
                ExecutionCommand::QueryExecution {
                    broker_order_id: "x".into(),
                },
            ),
            ("reconcile_balance", ExecutionCommand::ReconcileBalance),
            (
                "enter_manual_intervention",
                ExecutionCommand::EnterManualIntervention { reason: "x".into() },
            ),
            ("persist_state", ExecutionCommand::PersistState),
        ];
        let mut labels: Vec<&'static str> = variants.iter().map(|(l, _)| *l).collect();
        let total = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), total, "command_label 중복");
        for (expected, cmd) in variants {
            assert_eq!(command_label(&cmd), expected);
        }
    }

    #[test]
    fn event_label_is_stable_for_all_variants() {
        let variants = [
            ("signal_triggered", signal()),
            ("entry_order_accepted", entry_accepted()),
            (
                "execution_notice_received",
                ExecutionEvent::ExecutionNoticeReceived {
                    broker_order_id: "x".into(),
                    filled_qty: 0,
                    target_qty: 0,
                },
            ),
            (
                "execution_query_resolved",
                ExecutionEvent::ExecutionQueryResolved {
                    broker_order_id: "x".into(),
                    final_qty: 0,
                    target_qty: 0,
                },
            ),
            (
                "balance_reconciled",
                ExecutionEvent::BalanceReconciled { holdings_qty: 0 },
            ),
            (
                "exit_requested",
                ExecutionEvent::ExitRequested { reason: "x".into() },
            ),
            (
                "exit_order_accepted",
                ExecutionEvent::ExitOrderAccepted {
                    broker_order_id: "x".into(),
                },
            ),
            (
                "timeout_expired",
                ExecutionEvent::TimeoutExpired {
                    phase: TimeoutPhase::EntryFill,
                },
            ),
            ("manual_intervention_required", manual_required()),
        ];
        let mut labels: Vec<&'static str> = variants.iter().map(|(l, _)| *l).collect();
        let total = labels.len();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), total, "event_label 중복");
        for (expected, event) in variants {
            assert_eq!(event_label(&event), expected);
        }
    }
}
