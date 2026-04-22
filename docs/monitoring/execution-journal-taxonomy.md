# execution_journal Taxonomy (2026-04-19, 2 차 개정)

이 문서는 `execution_journal` 테이블에 기록되는 **phase 체계**, **시각 필드 사용 규칙**,
그리고 **운영 쿼리 패턴** 을 코드-문서 일치 기준으로 정리한다.

계획서 근거: `docs/monitoring/2026-04-19-execution-followup-plan.md` §3 / §4.

스키마 정의: `src/infrastructure/cache/postgres_store.rs` — `execution_journal` CREATE TABLE.

`append_execution_journal` 호출자: 전부 `src/strategy/live_runner.rs` 내부.

**코드-문서 single source of truth**: 재시작 복구 phase 는 `src/strategy/restart_recovery.rs`
의 `RestartAction::journal_phase()` 가 유일한 정의다. 새 phase 가 필요하면 해당 enum
에 variant 를 추가하거나 기존 variant 의 phase 매핑을 바꾸고, 이 문서를 같은 PR 에서
갱신해야 한다 (§5 변경 프로토콜 참고).

---

## 1. Phase 분류

phase 는 **주문 단위(order)** 와 **복구 단위(recovery)** 로 나뉜다. 이 구분은
운영 쿼리의 핵심이다. 같은 signal 이라도 주문 phase 는 `broker_order_id` 를 반드시
갖지만, 복구 phase 는 broker 에 실제 주문이 없을 수 있다.

### 1.1 Entry 라이프사이클 (주문 단위)

| phase | 발생 조건 | 시각 필드 기록 | severity |
|---|---|---|---|
| `entry_submitted` | 진입 지정가 주문 REST 제출 직후 | `sent_at` | info |
| `entry_acknowledged` | 주문번호 수신 + DB persist 완료 | `ack_at` | info |
| `entry_partial` | 30초 체결 대기 중 일부 체결 감지 | `first_fill_at` | info |
| `entry_final_fill` | 완전 체결 확정 | `first_fill_at`, `final_fill_at` | info |
| `entry_cancelled` | 30초 미체결 → 자동 취소 성공 | — | info |
| `entry_submit_failed` | REST 호출 자체 실패 (네트워크/타임아웃) | — | warning |
| `entry_submit_rejected` | API 응답 거절 (증거금/호가 제한 등) | — | warning |

### 1.2 Exit 라이프사이클 (주문 단위)

| phase | 발생 조건 | 시각 필드 | severity |
|---|---|---|---|
| `exit_submit_failed` | SL/시간스탑/수동 청산 주문 REST 실패 | — | warning |
| `exit_submit_rejected` | API 거절 | — | warning |
| `exit_final_fill` | 청산 완전 체결 확인 | `final_fill_at` | info |
| `exit_retry_submit_failed` | 잔량 재청산 실패 | — | warning |
| `exit_retry_still_remaining_manual` | 재청산 후에도 잔량 남음 → manual 전환 | — | critical |
| `exit_retry_final_fill` | 재청산 완전 체결 | `final_fill_at` | info |

### 1.3 재시작 복구 (복구 단위)

재시작 복구 경로에서는 broker 에 새 주문을 발주하지 않는 경우가 많고, 시각 필드는
이미 경과한 시점이라 **의도적으로 `None` 으로 둔다**. `broker_order_id` 는 기존 주문
번호 (cancel / 재사용 대상) 가 있으면 채우고, 없으면 공란.

PR #3.d 부터 `src/strategy/restart_recovery.rs` 의 `RestartAction` enum 이 taxonomy
의 single source of truth 다. `RestartAction::journal_phase()` 리턴값과 아래 표의
phase 이름이 어긋나면 taxonomy 가 깨진 것이다 — 단위 테스트 `journal_phases_match_taxonomy_set`
이 모듈 내부에서 이를 강제한다.

#### 1.3.1 Entry 재시작

| phase | 발생 조건 | RestartAction variant | severity |
|---|---|---|---|
| `entry_restart_auto_clear` | EntryPending + 잔고 0 + 주문 조회 OK → cancel 후 flat | `EntryPendingAutoClear` | info |
| `entry_restart_manual_intervention` | EntryPending + 잔고>0 또는 조회 실패/cancel 실패 → manual | `EntryPendingManualRestore` / `EntryPendingManualOnQueryFail` | critical |

#### 1.3.2 Exit 재시작

| phase | 발생 조건 | RestartAction variant | severity |
|---|---|---|---|
| `exit_restart_auto_flat` | ExitPending 재시작 + 잔고 0 → flat | `ExitPendingAutoFlat` | info |
| `exit_restart_manual_intervention` | ExitPending 재시작 + 잔고>0 → manual | `ExitPendingManualRestore` | critical |
| `exit_balance_lookup_failed_manual` | 청산 직후 verify 중 잔고 조회 실패 (재시작 아님) → manual | (라이브 verify 경로, RestartAction 무관) | critical |

#### 1.3.3 Open / Phantom 재시작

Open 상태로 정상 복구되거나, 재시작 중 KRX 가 TP 를 체결해 유령 상태가 된 경우.

| phase | 발생 조건 | RestartAction variant | severity |
|---|---|---|---|
| `open_restart_restore` | Open saved + 잔고>0 → 포지션 재구성 + 새 TP 지정가 발주 | `OpenPositionRestore` | info |
| `open_restart_phantom_cleanup` | Open saved + TP 취소 실패 + 잔고 0 (재시작 중 체결 추정) → DB 정리 | `OpenPhantomCleanup` | info |

#### 1.3.4 Orphan 재시작

DB `active_positions` 에 레코드가 없는 상태에서 시작된 경로.

| phase | 발생 조건 | RestartAction variant | severity |
|---|---|---|---|
| `orphan_restart_cleanup` | saved=None + 잔고 0 → cancel_all 후 깨끗이 시작 | `OrphanCleanup` | info |
| `orphan_restart_holding_detected` | saved=None + 잔고>0 → DB 메타 부재. 기존 TP 보존 + manual | `OrphanHoldingDetected` | critical |

#### 1.3.5 Manual / Fail-Safe 재시작

| phase | 발생 조건 | RestartAction variant | severity |
|---|---|---|---|
| `manual_restart_restore` | saved=ManualIntervention → DB 상태 그대로 복원 (자동화 차단 유지) | `ManualRestore` | critical |
| `restart_balance_lookup_failed_manual` | 잔고 조회 자체 실패 (saved 상태 무관) → fail-safe manual | `ManualFailSafe` | critical |

### 1.4 RestartAction ↔ phase 매핑 요약

| RestartAction | journal phase |
|---|---|
| `OrphanCleanup` | `orphan_restart_cleanup` |
| `OrphanHoldingDetected` | `orphan_restart_holding_detected` |
| `ManualFailSafe` | `restart_balance_lookup_failed_manual` |
| `ManualRestore` | `manual_restart_restore` |
| `EntryPendingManualRestore` | `entry_restart_manual_intervention` |
| `EntryPendingManualOnQueryFail` | `entry_restart_manual_intervention` |
| `EntryPendingAutoClear` | `entry_restart_auto_clear` |
| `ExitPendingManualRestore` | `exit_restart_manual_intervention` |
| `ExitPendingAutoFlat` | `exit_restart_auto_flat` |
| `OpenPositionRestore` | `open_restart_restore` |
| `OpenPhantomCleanup` | `open_restart_phantom_cleanup` |

N:1 매핑 주의: `EntryPendingManualRestore` 와 `EntryPendingManualOnQueryFail` 은
**같은 phase** 에 기록된다. 두 경우 모두 운영자가 "entry 재시작 manual" 건으로 동일
취급해야 하기 때문이다. `metadata.manual_category` 로 구체 분기를 구분한다 (§1.5 참고).

### 1.5 Manual 분기 필수 메타 표준

모든 재시작 manual phase (severity=critical) 는 운영자가 건별로 확인할 수 있도록
아래 **필수 메타** 를 `metadata` JSONB 에 포함해야 한다. PR #3.d 부터 `live_runner.rs`
경로에서 이 규약을 따르고, PR #3.f 부터 `restart_recovery::validate_restart_manual_metadata`
pure fn 으로 단위 테스트에서 누락을 자동 검증한다.

| 메타 키 | 의미 | 필수 범위 |
|---|---|---|
| `prior_execution_state` | DB 의 `active_positions.execution_state` 저장값 또는 `"absent"` (레코드 부재) | **모든 manual phase 필수** |
| `manual_category` | 서브 분기 라벨 — 전체 값은 아래 §1.5.1 참고 | **모든 manual phase 필수** |
| `pending_entry_order_no` | EntryPending 계열 전용 — 취소 대상 주문번호 | `entry_restart_manual_intervention` |
| `last_exit_order_no` | ExitPending 계열 전용 — 직전 청산 주문번호 | `exit_restart_manual_intervention` |
| `last_exit_reason` | ExitPending 계열 전용 — 직전 청산 사유 | `exit_restart_manual_intervention` |
| `quantity` | orphan 감지 잔고 수량 | `orphan_restart_holding_detected` |
| `stop_loss` / `take_profit` | manual 복원 시 유지할 브래킷 | `manual_restart_restore` |
| `holdings_qty` | 현재 잔고 수량 (0 일 수 있음) | `_holdings_remaining` 계열 권장 |
| `restored_stop_loss` / `restored_take_profit` | EntryPending 재구성 시 사용한 SL/TP | `entry_restart_manual_intervention` 권장 |
| `reason_tag` | 내부 분기 식별자 (테스트/지표 추적용) | 권장 |

#### 1.5.1 `manual_category` 표준값

현재 `live_runner.rs` 에서 실제 발생하는 값:

| manual_category | 발생 phase | 의미 |
|---|---|---|
| `post_cancel_balance_lookup_failed` | `entry_restart_manual_intervention` | cancel 성공 후 잔고 재확인 실패 |
| `cancel_failed` | `entry_restart_manual_intervention` | pending 주문 cancel 자체 실패 |
| `initial_balance_lookup_failed` | `entry_restart_manual_intervention` | 복구 시작 첫 잔고 조회 실패 |
| `entry_pending_holdings_remaining` | `entry_restart_manual_intervention` | 초기 잔고 확인 결과 보유 중 |
| `entry_pending_restart_cancel_race` | `entry_restart_manual_intervention` | cancel 와 체결 경쟁으로 잔고 발생 |
| `exit_pending_holdings_remaining` | `exit_restart_manual_intervention` | ExitPending 재시작 시 잔고 남음 |
| `exit_pending_balance_lookup_failed` | `exit_restart_manual_intervention` | ExitPending 재시작 잔고 조회 실패 |
| `orphan_cleanup` | `orphan_restart_cleanup` (info) | DB 레코드 없음 + 잔고 0 → cancel_all |
| `orphan_holding_detected` | `orphan_restart_holding_detected` | DB 레코드 없음 + 잔고 > 0 |
| `balance_lookup_failed` | `restart_balance_lookup_failed_manual` | 잔고 조회 자체가 실패 |
| `watchdog_restart_preserved` | `manual_restart_restore` | 직전 manual 상태 DB 그대로 복원 |

#### 1.5.2 `prior_execution_state` 표준값

| 값 | 의미 | 발생 phase 계열 |
|---|---|---|
| `"entry_pending"` | EntryPending 에서 재시작 | `entry_restart_*`, `manual_restart_restore`(drift) |
| `"entry_partial"` | 일부 체결 상태에서 재시작 | `entry_restart_*` |
| `"exit_pending"` | ExitPending 에서 재시작 | `exit_restart_*` |
| `"exit_partial"` | 부분 청산 상태에서 재시작 | `exit_restart_*` |
| `"open"` | 정상 Open 상태에서 재시작 | `open_restart_*`, `manual_restart_restore` |
| `"manual_intervention"` | 이전에 이미 manual 로 고정 | `manual_restart_restore` |
| `"absent"` | DB `active_positions` 에 레코드 자체가 없음 | `orphan_restart_*`, `restart_balance_lookup_failed_manual` |

**왜 이렇게 정의하는가**: 운영자가 manual 건을 보면 최소 아래 4 개 질문에 답할 수 있어야 한다.

1. 재시작 전에 어떤 상태였나 → `prior_execution_state`
2. 어느 분기 때문에 manual 로 갔나 → `manual_category` + `reason_tag`
3. 지금 KIS 에 남아 있는 주문이 뭔가 → `pending_entry_order_no` / `last_exit_order_no` / `broker_order_id`
4. 지금 실제 보유 수량이 몇 주인가 → `holdings_qty` 또는 `holdings_after` 또는 `quantity`

#### 1.5.3 Validator

`src/strategy/restart_recovery.rs::validate_restart_manual_metadata(phase, metadata)`
는 위 표에 정의된 **필수 키** 가 실제 metadata 에 포함되어 있는지 검증한다. 누락된
키 이름을 `Vec<&'static str>` 로 반환하며, 빈 Vec 이면 통과.

현재는 unit test 에서만 사용되고 runtime enforcement 는 하지 않는다. 메타 키가 늘어나거나
필수성이 바뀌면 이 함수와 본 §1.5 을 **같은 PR 에서 수정**한다.

### 1.6 Armed watch (주문 이전 단계)

2026-04-19 PR #2.e 부터 정식 기록. `armed_watch_budget_ms > 0` 일 때만 발생한다.
signal 이 발생한 뒤 주문 제출 이전 구간에서 drift/preflight/cutoff 등을 재확인하는
짧은 감시 구간이다.

| phase | 발생 조건 | 시각 필드 | severity |
|---|---|---|---|
| `armed_wait_enter` | `armed_wait_for_entry` 진입 직후 (signal 확정 + 중복 watcher 아님) | `sent_at` | info |
| `armed_wait_exit` | watcher 루프 종료 — outcome 에 따라 `ready`/`cutoff_reached`/`preflight_failed`/`drift_exceeded`/`stopped`/`aborted`/`already_armed` | `final_fill_at` | info 또는 warning (outcome 에 따라) |

`armed_wait_exit.resolution_source` 에 outcome label 이 들어간다 (`ready` / `cutoff_reached`
/ `preflight_failed` / `drift_exceeded` / `stopped` / `aborted` / `already_armed`).
`metadata.outcome`, `metadata.detail`, `metadata.duration_ms` 로 해당 watcher 의 라이프타임
전체를 재구성할 수 있다. `detail` 은 `PreflightFailed` / `Aborted` variant 에만 채워지며,
각각 `armed_preflight_subscription_stale` / `armed_preflight_spread_exceeded` /
`armed_preflight_nav_failed` / `armed_preflight_regime_unconfirmed` /
`armed_manual_intervention` / `armed_degraded` / `armed_entry_submit_timeout` 중 하나다.

중복 watcher 방지: 같은 `signal_id` 로 watcher 가 이미 돌고 있으면 `ArmedWaitOutcome::AlreadyArmed`
로 즉시 종료되며, 이 때도 `armed_wait_exit` 이 기록된다 (duration_ms=0).

**Single source of truth**: PR #2.f 부터 `src/strategy/armed_taxonomy.rs` 가 armed
watch 의 phase / event_log type / outcome label / detail reason 을 상수로 묶어
중앙 관리한다. `ArmedWaitOutcome::label()` 와 `OUTCOME_*` 상수가 어긋나지 않도록
`outcome_labels_match_armed_wait_outcome` 테스트가 양쪽을 강제한다. `validate_armed_exit_metadata`
/ `validate_armed_enter_metadata` pure fn 으로 metadata 필수 키 누락을 단위 테스트에서
자동 검출한다.

**Race-free wake-up** (PR #2.g, 2026-04-20): `armed_watch_notify: Arc<Notify>` 는
`armed_wake_tx: Arc<watch::Sender<u64>>` (generation counter) 로 교체되었다.

이전 구현 (PR #2.f) 은 `tokio::pin!(notified) + enable()` 패턴으로 "대부분의" race 를
닫았지만, select! 에서 notified branch 가 선택된 직후 `set(wake_notify.notified())`
로 새 future 를 만드는 순간부터 다음 iteration 의 `enable()` 까지의 짧은 창에 오는
`notify_waiters()` 는 여전히 유실될 수 있었다. 유실돼도 그 다음 state check 로
간접 감지되긴 했지만, "tick 대기 없이 즉시 종료" 를 100% 보장하지는 못했다.

`tokio::sync::watch` 채널은 Sender 가 `send_modify` 한 값이 영속적으로 보관되며,
`Receiver::changed()` 가 "이전에 본 값 이후 변경이 있으면 즉시 반환" 의미론 + cancel-safe
를 제공하므로 미등록 창 자체가 존재하지 않는다. `transition_to_degraded` /
`trigger_manual_intervention` 이 state 변경 직후 generation 을 올리면, watcher 가
이후 어느 시점에 `changed()` 를 호출하든 반드시 감지된다.

단위 테스트 `wake_channel_detects_update_after_mark_unchanged`, `wake_channel_cancel_safe_across_select`
가 두 핵심 race 시나리오를 고정한다.

### 1.7 Execution actor skeleton (PR #4 골격, wire-up 전)

2026-04-20 PR #4 는 `src/strategy/execution_actor.rs` 에 **pure function 상태 전이**
만 추가한 단계다. 실제 `live_runner.rs` 경로는 교체되지 않았고, 이 skeleton 은
후속 PR 에서 async actor task 로 확장된다.

**현재 존재하는 것**:

| 항목 | 위치 | 의미 |
|---|---|---|
| `ExecutionEvent` (9 variants) | `execution_actor.rs` | actor 가 소비하는 유일한 입력 |
| `ExecutionCommand` (7 variants) | `execution_actor.rs` | actor 가 caller 에 지시하는 side-effect 의도 |
| `TimeoutPhase` (3 variants) | `execution_actor.rs` | `TimeoutExpired` 의 세부 분류 |
| `apply_execution_event(state, event)` | `execution_actor.rs` | pure fn — `(ExecutionState, ExecutionEvent) → Result<ExecutionTransition, InvalidTransition>` |
| 28 단위 테스트 | `execution_actor::tests` | 각 state × event 전이 정당성 고정 |

**아직 기록되지 않는 phase** (후속 PR 에서 추가):

| phase | 추가 예정 PR | 의도 |
|---|---|---|
| `execution_actor_event_received` | 후속 | actor task 가 event 를 받은 시점 (state/event label/previous state 기록) |
| `execution_actor_command_issued` | 후속 | actor 가 command 를 반환한 시점 (command label + metadata) |
| `execution_actor_invalid_transition` | 후속 | 허용되지 않은 조합 — root cause 추적용 (severity=warning) |

### 1.8 Skeleton 에서 actor task 로 가는 길

1. **#5.a** (✅ 2026-04-20 완료) — `execution_actor::is_actor_allowed_transition`
   pure fn 추가. `transition_execution_state` 가 매 전이마다 shadow validation.
   mismatch 면 `event_log` 에 `actor_rule_mismatch` 기록 (severity=warning). 기존
   runtime 경로는 0 변경 — 회귀 위험 없이 "actor rule 로 설명 못 하는 전이 pair"
   를 프로덕션 데이터로 수집.
2. **#5.b** (✅ 2026-04-20 완료) — signal/entry 경로에 actor event dispatch 도입.
   `LiveRunner::dispatch_execution_event(event, reason)` helper 추가 —
   `apply_execution_event` 가 결정한 새 상태로 `transition_execution_state` 호출,
   command 는 runtime 실행하지 않고 `actor_command_intent` event_log 로만 기록.
   `execute_signal_armed` 경로가 `SignalTriggered` 이벤트로, 주문 접수 성공 경로가
   `EntryOrderAccepted` 이벤트로 전환. execute_entry 초반의 pre-submit EntryPending
   전이는 제거 (legacy `SignalArmed → EntryPending (execute_entry_start)` mismatch
   경로 해소). invalid event 는 `actor_invalid_transition` event_log.
3. **#5.c** (✅ 2026-04-20 완료) — exit 경로 actor dispatch + `ReconcileBalance`
   wiring. `close_position_market` 진입 시 `ExitRequested` dispatch (Open → ExitPending),
   `finalize_flat_transition` 의 Flat 전이를 `BalanceReconciled { holdings_qty: 0 }`
   dispatch 로 교체 ({ExitPending, ExitPartial} → Flat, 불변식 #2 반영).
   `dispatch_execution_event` 가 `ReconcileBalance` command 를 만나면
   `execute_reconcile_balance_command` helper 로 실제 `fetch_holding_snapshot`
   호출 + `event_log.actor_reconcile_balance` 기록 (성공/0주/조회실패 3경로).
   state 전이는 여기서 하지 않음 — self-dispatch recursion 회피. 다른 command
   (SubmitExitOrder 등) 은 여전히 intent-only.
4. **#5.d-design** (✅ 2026-04-20 완료, pure artifact only) — timeout 정책 설계.
   `execution_actor.rs` 에 `TimeoutPolicy { entry_submit, entry_fill, exit_fill }`
   + `paper_defaults()` (30s entry_fill, 기존 호환) / `real_defaults()` (계획서
   권장 2~3s) + `next_timeout_for(state, policy) -> Option<(TimeoutPhase, Duration)>`
   pure fn. 9 단위 테스트 (state × phase 매핑, policy 값 주입, 3 phase 완전성).
   **runtime reachable path 0** — `live_runner.rs` / `config.rs` / actor task /
   watch channel / journal phase 어느 곳에도 연결되지 않음. `#[allow(dead_code)]`
   로 lint 무시 표기. #5.c 운영 관측이 통과되기 전까지 wire-up 대기.
5. **#5.d-wire** — `TimeoutPolicy` 를 `LiveRunnerConfig` 에 연결 + actor task
   spawn + `next_timeout_for` 실 호출 + `TimeoutExpired` event dispatch 연결 +
   `armed_wait_for_entry` 와의 통합. #5.c 관측 통과 후 진행.
   ✅ 2026-04-21 구현: `LiveRunnerConfig.execution_timeout_policy()` /
   `exit_fill_timeout()` 추가, `armed_wait_for_entry` 가 `EntrySubmit` timeout 을
   소비, `execute_entry` 가 `EntryFill` timeout 시 `TimeoutExpired` 를 dispatch,
   `close_position_market` / `finalize_tp_partial_fill` / `handle_exit_remaining` 의
   `verify_exit_completion` 경로가 `ExitFill` outer timeout 으로 감싸짐.
   `dispatch_execution_event` 는 `EnterManualIntervention` command 를 runtime 실행으로
   연결해 timeout manual 전이가 실제 `stop_flag` / lock / DB manual 플래그까지
   반영된다. 현재는 기존 `live_runner` 가 I/O ownership 을 유지하는
   "owner-preserving wire" 단계이며, 완전한 async actor queue 는 #5.e 이후 과제.
6. **#5.e** — 모든 state 전이 경로 actor 일원화. `live_runner.rs` 가 actor 메시지
   기반 consumer 로 수축.

각 단계마다 이전 경로를 먼저 parity 테스트로 잠그고 전환한다 (§5 변경 프로토콜 준수).

#### 1.8.1 `actor_rule_mismatch` 운영 쿼리 (PR #5.a)

shadow validation 경고 수집용. mismatch 가 특정 from→to 에 반복적이라면 actor rule
에 event 추가 또는 whitelist 확장 필요.

```sql
SELECT
  metadata->>'from' AS from_state,
  metadata->>'to' AS to_state,
  metadata->>'reason' AS reason,
  COUNT(*) AS count,
  MAX(event_time) AS last_seen
FROM event_log
WHERE event_type = 'actor_rule_mismatch'
  AND event_time > NOW() - INTERVAL '7 days'
GROUP BY from_state, to_state, reason
ORDER BY count DESC, last_seen DESC;
```

현 whitelist (실 운영에서 자주 발생하는 패턴이면 본 rule 로 편입 검토):

- `Degraded ↔ {Open, Flat, SignalArmed}` — preflight health 기반 외부 전이.

mismatch 로 잡힐 가능성 있는 패턴 (현재 live_runner.rs 실제 호출 분석 결과, 후속
PR 에서 rule 로 편입 또는 호출부 정정):

- ~~`Flat → EntryPending` / `SignalArmed → EntryPending (execute_entry_start)` — ✅
  PR #5.b 에서 actor 경유로 교체 완료, 해소됨.~~
- `EntryPartial → ExitPartial` (TP 부분 체결 경로).

---

## 2. 시각 필드 매트릭스

`sent_at`, `ack_at`, `first_fill_at`, `final_fill_at` 은 주문 라이프사이클 phase
에서만 채워진다. 복구 phase 에서는 전부 `None` — 재시작 시점에는 이미 원래 시점이
지났기 때문.

| phase | sent_at | ack_at | first_fill_at | final_fill_at |
|---|---|---|---|---|
| `entry_submitted` | ✅ | | | |
| `entry_acknowledged` | | ✅ | | |
| `entry_partial` | | | ✅ | |
| `entry_final_fill` | | | ✅ | ✅ |
| `entry_cancelled` | | | | |
| `entry_submit_failed` | | | | |
| `entry_submit_rejected` | | | | |
| `exit_submit_failed` | | | | |
| `exit_submit_rejected` | | | | |
| `exit_final_fill` | | | | ✅ |
| `exit_retry_submit_failed` | | | | |
| `exit_retry_still_remaining_manual` | | | | |
| `exit_retry_final_fill` | | | | ✅ |
| `armed_wait_enter` | ✅ | | | |
| `armed_wait_exit` | | | | ✅ |
| `entry_restart_*` | — | — | — | — |
| `exit_restart_*` | — | — | — | — |
| `open_restart_*` | — | — | — | — |
| `orphan_restart_*` | — | — | — | — |
| `manual_restart_restore` | — | — | — | — |
| `restart_balance_lookup_failed_manual` | — | — | — | — |
| `exit_balance_lookup_failed_manual` | — | — | — | — |

**활용 기준**:

- `sent_at`: 주문 제출 latency 측정의 시작점 (제출 → 브로커 ack).
- `ack_at`: 브로커 응답 latency 측정의 종료점. `ack_at - sent_at` 은 REST round-trip.
- `first_fill_at`: 체결 지연 시작 (ack → 첫 체결). 지정가 호가 경쟁 지표.
- `final_fill_at`: 주문 완료 시각. 부분 체결이 있었다면 `first_fill_at` 이 먼저.

partial → final 시차가 크면 유동성 부족 또는 가격 이탈 가능성.

---

## 3. `execution_id` / `broker_order_id` 사용 규칙

### 3.1 `execution_id`

signal 1 회에 대응하는 논리 식별자. 포맷:

```
{stock_code}:{HHMMSS}:{entry_price}:{quantity}
```

생성: `LiveRunner::signal_execution_id(signal_time, entry_price, quantity)`.

한 signal 의 모든 phase (submit → ack → fill → exit) 는 같은 `execution_id` 를
공유한다. 재시작 복구 경로도 원래 저장된 signal 기준으로 같은 값을 쓴다.

### 3.2 `broker_order_id`

KIS 에서 발급한 주문번호 (odno). 포맷: 숫자 10자리 문자열. 주문 단위 phase 에서만 채움.

- 동일 signal 이라도 진입/청산은 주문이 분리되어 `broker_order_id` 가 **다르다**.
- 재시작 복구 중 cancel 대상 주문번호가 있으면 `entry_restart_*` 에도 기록.
- partial → final 은 같은 주문이라 같은 값.

### 3.3 조회 경로별 사용

| 조회 목적 | 키 | 주의 |
|---|---|---|
| "이 signal 이 어떤 라이프사이클을 탔나" | `execution_id` | 복구 phase 포함됨 |
| "이 브로커 주문번호의 체결 내역" | `broker_order_id` | 주문 phase 만 |
| "오늘 전체 진입 흐름" | `phase LIKE 'entry_%'` + `DATE(created_at)` | 복구도 포함 |
| "오늘 manual 전환 원인" | `phase LIKE '%manual%'` | 재시작 manual 5 개 + retry manual + 청산 verify manual 매칭 |
| "재시작 복구 전수 조회" | `phase LIKE '%_restart_%'` OR `phase = 'restart_balance_lookup_failed_manual'` | 10 개 phase 매칭 |

### 3.4 재시작 phase 의 ID 사용 특이사항

재시작 복구 경로에서는 새 주문을 발주하지 않으므로 `execution_id` / `broker_order_id`
를 아래 규칙으로 채운다.

| phase | `execution_id` | `broker_order_id` |
|---|---|---|
| `entry_restart_*` | 취소 대상 `pending_entry_order_no` | 동일 |
| `exit_restart_*` | 직전 `last_exit_order_no` | 동일 |
| `open_restart_restore` | `saved.signal_id` (없으면 빈문자열) | 새로 발주된 TP 주문번호 |
| `open_restart_phantom_cleanup` | `saved.signal_id` | 취소 시도한 TP 주문번호 |
| `manual_restart_restore` | `saved.signal_id` | 기존 TP 주문번호 (있다면) |
| `orphan_restart_*` | 빈 문자열 | 빈 문자열 |
| `restart_balance_lookup_failed_manual` | 빈 문자열 | 빈 문자열 |

orphan / balance fail-safe 경로가 빈문자열인 이유: DB 에 메타가 없기 때문에 상관관계를
만들어 낼 근거가 아예 없다. `created_at` + `stock_code` 만으로 조회한다.

---

## 4. 운영 쿼리 예시

**오늘의 실행 요약**:
```sql
SELECT phase, COUNT(*), MIN(created_at), MAX(created_at)
FROM execution_journal
WHERE DATE(created_at) = CURRENT_DATE
GROUP BY phase
ORDER BY MIN(created_at);
```

**한 signal 의 전체 라이프사이클**:
```sql
SELECT created_at, phase, side, filled_qty, sent_at, ack_at, first_fill_at, final_fill_at,
       broker_order_id, reason, error_message
FROM execution_journal
WHERE execution_id = $1
ORDER BY created_at;
```

**REST 제출-인수 지연 분포**:
```sql
SELECT EXTRACT(EPOCH FROM (ack_at - sent_at)) * 1000 AS ack_latency_ms
FROM execution_journal
WHERE phase = 'entry_acknowledged'
  AND sent_at IS NOT NULL AND ack_at IS NOT NULL
  AND DATE(created_at) = CURRENT_DATE;
```

**Manual 전환 원인 탐색** (운영자 수동 확인 필요 건):
```sql
SELECT created_at, stock_code, phase, reason, error_message, metadata
FROM execution_journal
WHERE phase IN (
  'entry_restart_manual_intervention',
  'exit_restart_manual_intervention',
  'exit_retry_still_remaining_manual',
  'exit_balance_lookup_failed_manual'
)
ORDER BY created_at DESC
LIMIT 20;
```

**재시작 복구 통계 (주간)**:
```sql
SELECT phase, COUNT(*)
FROM execution_journal
WHERE (phase LIKE '%_restart_%' OR phase = 'restart_balance_lookup_failed_manual')
  AND created_at > NOW() - INTERVAL '7 days'
GROUP BY phase
ORDER BY COUNT(*) DESC;
```

**재시작 manual 전환 manual_category 별 분포** (운영자 루트 원인 추적):
```sql
-- manual_category 전체 값은 taxonomy §1.5.1, prior_execution_state 는 §1.5.2 참고.
-- PR #3.f 부터 모든 manual phase 에 두 키가 보장된다.
SELECT
  metadata->>'manual_category' AS manual_category,
  metadata->>'prior_execution_state' AS prior_state,
  COUNT(*) AS count,
  MAX(created_at) AS last_seen
FROM execution_journal
WHERE phase IN (
    'entry_restart_manual_intervention',
    'exit_restart_manual_intervention',
    'orphan_restart_holding_detected',
    'manual_restart_restore',
    'restart_balance_lookup_failed_manual'
  )
  AND created_at > NOW() - INTERVAL '30 days'
GROUP BY manual_category, prior_state
ORDER BY count DESC, last_seen DESC;
```

**Missing metadata 진단** (PR #3.f 이전 기록이나 신규 phase 누락 감지):
```sql
-- manual phase 인데 필수 키가 NULL 인 건 추출.
SELECT created_at, phase, stock_code,
       metadata->>'manual_category' AS manual_category,
       metadata->>'prior_execution_state' AS prior_state,
       metadata
FROM execution_journal
WHERE phase IN (
    'entry_restart_manual_intervention',
    'exit_restart_manual_intervention',
    'orphan_restart_holding_detected',
    'manual_restart_restore',
    'restart_balance_lookup_failed_manual'
  )
  AND (metadata->>'manual_category' IS NULL
       OR metadata->>'prior_execution_state' IS NULL)
ORDER BY created_at DESC
LIMIT 50;
```

**Stale pending entry 감지** (지금 DB 에 `entry_pending` 인데 최근 24 시간 entry_restart 관련 기록이 없는 주문 — 청소 누락 의심):
```sql
SELECT ap.stock_code, ap.pending_entry_order_no, ap.entry_time,
       ap.manual_intervention_required
FROM active_positions ap
WHERE ap.execution_state = 'entry_pending'
  AND NOT EXISTS (
    SELECT 1 FROM execution_journal ej
    WHERE ej.stock_code = ap.stock_code
      AND ej.broker_order_id = ap.pending_entry_order_no
      AND ej.phase IN (
        'entry_submitted', 'entry_acknowledged', 'entry_partial', 'entry_final_fill',
        'entry_cancelled', 'entry_restart_auto_clear', 'entry_restart_manual_intervention'
      )
      AND ej.created_at > NOW() - INTERVAL '24 hours'
  );
```

**Armed watch outcome 분포** (PR #2.e):
```sql
-- 최근 24시간 종목별 armed_wait_exit outcome 카운트 + 평균 duration.
SELECT
  stock_code,
  metadata->>'outcome' AS outcome,
  metadata->>'detail' AS detail,
  COUNT(*) AS count,
  AVG((metadata->>'duration_ms')::bigint) AS avg_duration_ms,
  MAX((metadata->>'duration_ms')::bigint) AS max_duration_ms
FROM execution_journal
WHERE phase = 'armed_wait_exit'
  AND created_at > NOW() - INTERVAL '24 hours'
GROUP BY stock_code, outcome, detail
ORDER BY stock_code, count DESC;
```

**Armed watch 중복 발생 감지** (동일 signal 에 중복 watcher 시도가 있었는지):
```sql
SELECT execution_id, stock_code, COUNT(*) AS enter_count, MIN(created_at) AS first_seen
FROM execution_journal
WHERE phase = 'armed_wait_enter'
  AND created_at > NOW() - INTERVAL '24 hours'
GROUP BY execution_id, stock_code
HAVING COUNT(*) > 1
ORDER BY first_seen DESC;
```

즉시 노출이 필요하면 `GET /api/v1/monitoring/armed-stats` 를 사용한다. 러너별
`ArmedWatchStats` (카운터 + ready/aborted 평균) 를 실시간 반환한다.

**응답 예시**:

```json
[
  {
    "stock_code": "122630",
    "stats": {
      "ready_count": 12,
      "cutoff_count": 0,
      "preflight_failed_count": 1,
      "drift_exceeded_count": 2,
      "stopped_count": 0,
      "aborted_count": 1,
      "already_armed_count": 0,
      "total_duration_ms": 2840,
      "max_duration_ms": 210,
      "ready_duration_ms_sum": 2150,
      "ready_duration_ms_max": 210,
      "aborted_duration_ms_sum": 18,
      "aborted_duration_ms_max": 18
    },
    "current_armed_signal_id": null,
    "manual_intervention_required": false,
    "degraded": false,
    "execution_state": "flat",
    "ready_avg_duration_ms": 179.16666666666666,
    "aborted_avg_duration_ms": 18.0
  },
  {
    "stock_code": "114800",
    "stats": {
      "ready_count": 0, "cutoff_count": 0, "preflight_failed_count": 0,
      "drift_exceeded_count": 0, "stopped_count": 0, "aborted_count": 0,
      "already_armed_count": 0, "total_duration_ms": 0, "max_duration_ms": 0,
      "ready_duration_ms_sum": 0, "ready_duration_ms_max": 0,
      "aborted_duration_ms_sum": 0, "aborted_duration_ms_max": 0
    },
    "current_armed_signal_id": null,
    "manual_intervention_required": false,
    "degraded": false,
    "execution_state": "flat",
    "ready_avg_duration_ms": null,
    "aborted_avg_duration_ms": null
  }
]
```

해석 팁:
- `aborted_duration_ms_max` 가 `armed_tick_check_interval_ms` (기본 150ms) 에 근접하면
  race-free wake-up (PR #2.f) 이 제대로 동작하는 것 — notify 없이 tick 폴링까지 기다린
  상한이 이 값이다. 훨씬 작으면 notify path 가 잘 도는 것이고, 훨씬 크면 wait_for
  clamp 또는 다른 bottleneck 조사 필요.
- `drift_exceeded_count` 가 지속적으로 누적되면 signal 가격 산출 시점과 armed watch
  재확인 시점 사이 시장 변동이 크다는 뜻 — FVG detection 창 조정 고려.
- `already_armed_count > 0` 은 동일 signal 로 `armed_wait_for_entry` 가 두 번 호출된
  적이 있다는 신호. 정상 경로에서는 0 이어야 한다 — duplicate 호출 경로 조사 필요.

**체결 지연 이상치** (partial → final 이 길었던 건):
```sql
SELECT execution_id, broker_order_id,
       EXTRACT(EPOCH FROM (final_fill_at - first_fill_at)) AS partial_gap_sec
FROM execution_journal
WHERE phase = 'entry_final_fill'
  AND first_fill_at IS NOT NULL AND final_fill_at IS NOT NULL
  AND final_fill_at - first_fill_at > INTERVAL '2 seconds'
ORDER BY partial_gap_sec DESC;
```

---

## 4-A. Stale Pending Entry 정리 기준

### 정의

"Stale pending entry" 란 `active_positions.execution_state = 'entry_pending'` 인 DB
레코드가 남아 있는데, 아래 중 하나에 해당하는 상태를 말한다.

- KIS 에서는 이미 체결 또는 취소된 주문
- KIS 에는 여전히 미체결이지만 긴 시간 (≥ 24 시간 + 1 영업일) 동안 진행 없는 주문
- `pending_entry_order_no` 가 비어 있거나 KIS 가 모르는 주문번호

### 자동 판정 경로 (runner 재시작 시)

`check_and_restore_position` → `restore_entry_pending_state` 에서 `classify_restart_action`
의 판정 규칙과 동일한 순서로 자동 분류된다.

| 관측 조합 | RestartAction | phase |
|---|---|---|
| 잔고>0 | `EntryPendingManualRestore` | `entry_restart_manual_intervention` |
| 잔고=0 + 주문조회 OK | `EntryPendingAutoClear` | `entry_restart_auto_clear` |
| 잔고=0 + 주문조회 실패 | `EntryPendingManualOnQueryFail` | `entry_restart_manual_intervention` |
| 잔고 조회 자체 실패 | `ManualFailSafe` | `restart_balance_lookup_failed_manual` |

즉, **자동 정리 경로는 `entry_restart_auto_clear` 하나 뿐**이며, 그 외는 전부 manual.
이는 "불확실하면 운영자 확인" 불변식 (§5, 실행 계획 §핵심 불변식 5) 을 따른다.

### 수동 정리 기준 (운영자)

위 감지 쿼리 (§4 마지막) 로 stale pending 을 발견했고 runner 재시작이 불가능한 경우:

1. HTS/MTS 에서 해당 주문번호를 조회 → 체결/취소/여전히 미체결 중 하나 확정.
2. 미체결이면 HTS/MTS 에서 직접 취소.
3. DB 에서 `active_positions` 레코드 삭제 — 단, `manual_intervention_required = true`
   가 켜져 있다면 건드리지 말고 운영 노트에 근거 기록 후 해제.
4. `execution_journal` 에는 수동 처리 기록을 남기지 않는다 (이 경로는 자동이 아님).
   대신 운영 로그에 주문번호, 조회 결과, 조치 내용을 기록한다.

### Automated recovery vs manual recovery 의 경계

| 상황 | 운영자 개입 필요? |
|---|---|
| runner 재시작 → auto_clear 기록됨 | 아니오 (자동 처리 완료) |
| runner 재시작 → manual 기록됨 | 예 — manual_category 확인 후 §1.5 필수 메타로 원인 추적 |
| runner 기동 불가 + stale 감지 | 예 — 위 수동 정리 기준 적용 |
| `orphan_restart_holding_detected` 기록 | 예 — 미지의 잔고 출처 확인 (과거 runner 가 DB save 전 크래시한 흔적) |

---

## 5. 변경 프로토콜

phase 추가/삭제/리네이밍은 이 문서와 관련 코드를 **같은 PR 에서** 수정한다.
문서만 바꾸거나 코드만 바꾸면 taxonomy 가 깨진다.

### 재시작 복구 phase

`src/strategy/restart_recovery.rs` 의 `RestartAction::journal_phase()` 가 single source
of truth 다. 변경 절차:

1. `restart_recovery.rs` 에서 variant 추가 또는 `journal_phase()` 매핑 변경
2. `tests::journal_phases_match_taxonomy_set` 의 `allowed` 집합에 새 phase 추가
3. 이 문서 §1.3, §1.4, §2, §3.4 에 반영
4. `live_runner.rs` 에서 해당 action 을 실제 분기에서 사용
5. `cargo test --lib restart_recovery` 로 phase 집합 일치 확인

### 주문 라이프사이클 phase (submit / ack / fill / cancel)

1. `live_runner.rs` 에서 phase 문자열을 직접 사용 (`ExecutionJournalEntry.phase`)
2. 이 문서 §1.1 / §1.2 / §2 에 반영
3. 사후 쿼리 (§4) 호환성 확인 — 기존 phase 이름 변경 시 운영 대시보드 영향

### 공통 체크리스트

- [ ] 이 문서 §1 표에 추가/수정
- [ ] §2 시각 필드 매트릭스에 추가
- [ ] 재시작 phase 라면 `RestartAction::journal_phase()` + 단위 테스트 반영
- [ ] 라이프사이클 phase 라면 `live_runner.rs` 에서 실제 phase 문자열 사용
- [ ] 사후 쿼리 (§4) 호환성 확인
- [ ] 문서 상단 날짜/PR 번호 갱신

### Severity 규칙

- `info` — 정상 라이프사이클
- `warning` — 자동 재시도 가능한 실패 (submit_failed, rejected 등)
- `critical` — 수동 개입 필요 (manual_intervention, balance_lookup_failed)

---

## 6. 참고

- 스키마: `src/infrastructure/cache/postgres_store.rs`
- 호출자: `src/strategy/live_runner.rs`
- 계획서: `docs/monitoring/2026-04-19-execution-followup-plan.md` §4
- 관련 postmortem: `docs/monitoring/2026-04-17-trading-postmortem.md` (manual 오승격 사고)
- 관련 PR:
  - #3.a — 이 문서 최초 작성
  - #3.b — 재시작 판정 pure fn (`RestartAction`, `classify_restart_action`)
  - #3.c — 매트릭스 단위 테스트 (20+ 케이스)
  - #3.d — `RestartAction::journal_phase()` 추가 + `live_runner.rs` wire-up + 누락된 4 경로 journal 기록 (Open restore / phantom cleanup / manual restore / orphan 3 분기)
  - #3.e — 이 문서 §1.3 세분화 + §1.4 매핑표 + §1.5 manual 메타 표준 + §3.4 ID 규칙 + §4 재시작 쿼리 + §4-A stale pending 정리 기준
  - #3.f — manual 메타 정합성 마감: Exit manual phase `manual_category` 보강, entry `manual_category` 하드코딩 버그 수정, orphan/balance-fail 3 경로 `prior_execution_state="absent"` 추가, `validate_restart_manual_metadata` pure fn + 11개 단위 테스트, §1.5 `manual_category`/`prior_execution_state` 표준값 표 + missing-metadata 진단 쿼리
  - #2.e — armed watch 운영 보강: `armed_wait_enter` / `armed_wait_exit` phase 정식 기록 (§1.6), `ArmedWatchStats` outcome 별 duration 세분화 (ready/aborted sum+max+avg), manual/degraded 전이 시 `armed_watch_notify` 로 tick 주기 없이 즉시 종료, `GET /api/v1/monitoring/armed-stats` 엔드포인트로 이유별 카운터 노출, §4 armed watch 운영 쿼리 2 개
  - #2.f — armed wake-up race 축소 + taxonomy 상수화: `src/strategy/armed_taxonomy.rs` 신규 (JOURNAL_PHASE_*, EVENT_LOG_TYPE_*, OUTCOME_*, DETAIL_*) + `validate_armed_exit_metadata` / `validate_armed_enter_metadata` pure fn + 10 단위 테스트 (outcome label cross-check 포함), `armed_wait_for_entry` 에 pre-registered `notified()` 재생성 패턴으로 wake-up race 축소, `ArmedWaitOutcome::label()` 및 preflight/aborted detail 리터럴을 상수 참조로 교체, §1.6 single source of truth + race-free wake-up 설명 + armed-stats REST 응답 예시 추가 (단, `set() → enable()` 사이 미등록 창의 잔여 race 는 #2.g 에서 해결)
  - #2.g — wake-up race 완전 제거: `armed_watch_notify: Arc<Notify>` → `armed_wake_tx: Arc<watch::Sender<u64>>` 교체. `transition_to_degraded` / `trigger_manual_intervention` 이 `send_modify(|g| *g = g.wrapping_add(1))` 로 generation 을 올리고, `armed_wait_for_entry` 는 `subscribe()` + `mark_unchanged()` + `changed()` (cancel-safe) 로 감지. Tokio watch 의 영속 generation 덕분에 미등록 창 자체가 존재하지 않으므로 signal 유실 0. `wake_channel_detects_update_after_mark_unchanged` + `wake_channel_cancel_safe_across_select` 단위 테스트 2 개로 lock-in.
  - #4 — execution actor 최소 골격: `src/strategy/execution_actor.rs` 신규 (pure fn skeleton). `ExecutionEvent` 9 variants + `ExecutionCommand` 7 variants + `TimeoutPhase` 3 variants + `apply_execution_event(state, event) → Result<ExecutionTransition, InvalidTransition>` pure fn. 상태 9개 × 이벤트 9개 조합 중 허용 전이만 map, 나머지는 `InvalidTransition` 명시 에러. `ManualIntervention` 은 모든 이벤트 흡수 (자동 탈출 금지), `Degraded` 는 신규 `SignalTriggered` 만 무음 거부. 28 단위 테스트 (flat/signal_armed/entry_pending/entry_partial/open/exit_pending/exit_partial/manual/degraded 각 경로 + event_label cross-check). 이번 PR 은 wire-up 없음 — `live_runner.rs` 경로 변경 0. §1.7 skeleton 현황 + §1.8 actor task 이관 로드맵 (#5.a~#5.e) 추가.
  - #5.a — actor rule shadow validation: `execution_actor::is_actor_allowed_transition(prev, new) -> bool` pure fn 추가 (Degraded 외부 전이는 whitelist, 나머지는 15 event 샘플 enumerate). `transition_execution_state` 가 매 전이마다 호출하여 mismatch 시 `event_log.actor_rule_mismatch` 기록. runtime 경로 0 변경 (회귀 위험 없음) — 목적은 프로덕션 데이터 수집으로 actor rule 확장/정정 포인트 식별. 7 단위 테스트 (happy path entry/exit + manual escalation + timeout recovery + degraded whitelist + 알려진 invalid pair 거부). §1.8.1 운영 쿼리 + 예상 mismatch 패턴 목록.
  - #5.b — signal/entry 경로 actor dispatch 도입: `LiveRunner::dispatch_execution_event(event, reason)` helper (apply_execution_event 으로 state 결정, command 는 `actor_command_intent` event_log intent-only 기록, InvalidTransition 은 `actor_invalid_transition` warning). `ExecutionCommand::command_label()` pub fn 추가. signal 탐지 직후 `SignalTriggered` dispatch (Flat → SignalArmed), 주문 접수 성공 직후 `EntryOrderAccepted` dispatch (SignalArmed → EntryPending). execute_entry 초반 pre-submit `EntryPending` 전이는 제거 — 이전에 예상 mismatch 패턴으로 잡혔던 `SignalArmed → EntryPending (execute_entry_start)` 해소. 1 신규 단위 테스트 (command_label cross-check).
  - #5.c — exit 경로 actor dispatch + `ReconcileBalance` wiring: `close_position_market` 진입의 `ExitPending` 전이를 `ExitRequested` dispatch 로 교체 (Open → ExitPending). `finalize_flat_transition` 의 `Flat` 전이를 `BalanceReconciled { holdings_qty: 0 }` dispatch 로 교체 ({ExitPending, ExitPartial} → Flat, 불변식 #2 반영). `execute_reconcile_balance_command` helper 신규 — `dispatch_execution_event` 가 `ReconcileBalance` command 를 만나면 실제 `fetch_holding_snapshot` 호출 후 `event_log.actor_reconcile_balance` 에 holdings_qty/avg_price 기록 (성공/0주/조회실패 3경로). state 전이는 여기서 수행 안 함 — self-dispatch recursion 차단. 다른 command (SubmitExitOrder/CancelOrder 등) 는 여전히 intent-only.
  - #5.d-design — timeout 정책 pure artifact: `execution_actor.rs` 에 `TimeoutPolicy { entry_submit, entry_fill, exit_fill }` 구조체 + `paper_defaults` (30s entry_fill, 기존 호환) / `real_defaults` (계획서 권장 2~3s) + `next_timeout_for(state, policy) -> Option<(TimeoutPhase, Duration)>` pure fn. `TimeoutPhase` 에 `Hash` derive 추가 (완전성 테스트용). 9 단위 테스트 (state × phase 매핑, paper/real default 값, policy 값 주입 경로, 3 phase 완전성, Flat/Open/Manual/Degraded 는 None). **runtime reachable path 0** — `live_runner.rs` / `config.rs` / actor task / watch channel / journal phase 어느 곳에도 연결되지 않음. `#[allow(dead_code)]` 로 의도 명시. wire-up 은 #5.d-wire 에서 별도 분리, 조건: #5.c 운영 관측 통과.
  - #5.d-wire — timeout 정책 runtime 연결: `LiveRunnerConfig.execution_timeout_policy()` / `exit_fill_timeout()` 으로 설계 artifact 를 실제 설정값과 연결. `armed_wait_for_entry` 는 `SignalArmed → EntrySubmit` timeout 을 소비하고, `execute_entry` 는 `EntryPending` 체결 대기 timeout 시 `TimeoutExpired { EntryFill }` dispatch 후 기존 cancel/verify 루프를 계속 수행한다. `verify_exit_completion` 호출부(`close_position_market`, `finalize_tp_partial_fill`, `handle_exit_remaining`) 는 `ExitFill` outer timeout 으로 감싸져, budget 초과 시 `TimeoutExpired { ExitFill }` → `EnterManualIntervention` command → 실제 `trigger_manual_intervention` 까지 이어진다. 이 단계는 기존 `live_runner` 가 주문 I/O ownership 을 유지하는 owner-preserving wire 이며, full async actor queue 는 후속 단계.
