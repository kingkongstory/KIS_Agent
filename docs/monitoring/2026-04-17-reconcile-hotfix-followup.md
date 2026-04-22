# 2026-04-17 reconcile 후속 핫픽스 계획 (v3 follow-up)

> v2(`2026-04-17-reconcile-hotfix-plan.md`)로 P0/P1 적용 완료. 본 문서는 자체
> 코드리뷰에서 드러난 4가지 잔존 결함을 닫는 follow-up. 4건 적용 후 "기본
> reconcile 오판 손실 문제는 해결됐다" 라고 말할 수 있는 마감 단계.

## Context

v2 hotfix는 reconcile 강제 청산 → 슬리피지 손실 회로(`-0.17%` 사고)는 정확히
차단했다. 그러나 **자체 비판 검증**에서 드러난 결함:

| # | 결함 | 영향 |
|---|---|---|
| F1 | `confirm_reconcile_qty_with` 가 fetch error 한 번에 즉시 propagate. 호출부는 1차 샘플로 fallback → mismatch 판정 가능 | KIS API 일시 5xx/timeout 시 false manual stop 발생 |
| F2 | `manage_position` / 메인 루프 manual 스킵 분기가 `save_active_position` 을 호출하지 않음. 트레일링/본전스탑 진행 후 reconcile mismatch 발생 시 DB 행이 entry 시점 스냅샷 그대로 stale | 운영자 수동 처리 또는 재시작 복구 시 진행된 SL/best_price/reached_1r 사라짐 |
| F3 | reconcile_once 의 manual 전환 (`strategy.rs:947-984`) 이 `LiveRunner::trigger_manual_intervention` (`live_runner.rs:903-934`) 과 분리. shared `position_lock` 처리 누락 (다른 종목 진입 미차단) | 정책 코드 갈라짐, 다음 수정에서 invariant 깨질 위험. dual-locked 운용 시 unresolved live position 과 다른 종목 진입 중첩 가능 |
| F4 | 핵심 사고 경로(메인 루프 stop+manual+position=Some → close_position_market 미호출) 직접 회귀 테스트 부재. 현재 테스트는 helper 와 `current_position=None` 인 manage_position 만 검증 | 다음 리팩터에서 P0 회귀 시 못 잡음 |

## 우선순위 (사용자 합의)

1. **F1** — reconcile 재확인 루프 보강 (false manual stop 방지). 가장 즉각적
2. **F2** — manual 스킵 시 active_position 재저장 (재시작 복구 정합성)
3. **F3** — manual 전환 공용 helper (정책 일원화)
4. **F4** — 회귀 테스트 보강

## 구현 세부

### F1 — `confirm_reconcile_qty_with` 강화

**현재 (`strategy.rs:1043-1078`)**: 첫 fetch error 즉시 `Err` propagate. 호출부
(`strategy.rs:937-947`)는 `Err` 시 1차 샘플(`initial_kis_qty`)을 사용. 즉
KIS API 가 한 번이라도 5xx/네트워크 오류면 stale 1차 샘플로 mismatch 판정.

**수정 후 동작**:
- 매 attempt 마다 fetch 시도
- 성공 시: `expected_qty` 일치하면 **즉시 `Ok(qty)` 반환** (early return)
- 성공 시 mismatch: `last_successful_qty = Some(qty)` 누적, 다음 attempt 진행
- 실패 시: 에러를 누적, 다음 attempt 진행 (즉시 propagate 안 함)
- 모든 attempt 종료:
  - `last_successful_qty = Some(q)` → `Ok(q)` (마지막 성공값으로 mismatch 판정)
  - `last_successful_qty = None` (전부 fetch 실패) → `Err("재확인 모두 실패: ...")`

**시그니처 동일** (`expected_qty`, `attempts`, `interval`, `fetch`).

**핵심 코드 (`strategy.rs:1043-`)**:
```rust
pub(crate) async fn confirm_reconcile_qty_with<F, Fut>(
    expected_qty: u64,
    attempts: u32,
    interval: std::time::Duration,
    mut fetch: F,
) -> Result<u64, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<u64, String>>,
{
    let attempts = attempts.max(1);
    let mut last_successful_qty: Option<u64> = None;
    let mut errors: Vec<String> = Vec::new();
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        match fetch().await {
            Ok(qty) => {
                last_successful_qty = Some(qty);
                if qty == expected_qty {
                    return Ok(qty); // 회복 즉시 종료
                }
            }
            Err(e) => {
                errors.push(format!("attempt {}: {e}", attempt + 1));
            }
        }
    }
    match last_successful_qty {
        Some(q) => Ok(q), // 일부 성공 + 모두 mismatch → 마지막 성공 값
        None => Err(format!("재확인 모든 attempt 실패: {}", errors.join("; "))),
    }
}
```

### F1.1 — 호출부 (`reconcile_once`) inconclusive 처리

**현재 (`strategy.rs:937-947`)**: `Err` 시 `initial_kis_qty` (1차 샘플) 사용.
이 fallback 자체가 stale 위험.

**수정 후**:
- `Err` 시 → 종목별 `inconclusive_streak` 를 1 증가
- `inconclusive_streak < 3` 이면 `balance_reconcile_inconclusive` warn 이벤트 기록
- 이 경우 이번 cycle 에서는 manual 전환 안 함, 다음 cycle (1분 후) 재시도
- `inconclusive_streak >= 3` 이면 더 이상 "잠시 장애"로 보지 않고
  **상태 불명 manual_intervention 으로 승격**
- 어떤 형태로든 재확인 fetch 가 1회라도 성공하면 `inconclusive_streak = 0` 으로 리셋
- `continue` 로 다음 종목 진행

**핵심 코드 (`strategy.rs:937-`)**:
```rust
let confirmed_kis_qty = match self
    .confirm_reconcile_qty(&client, &code, runner_qty)
    .await
{
    Ok(qty) => qty,
    Err(e) => {
        let streak = self.bump_reconcile_inconclusive_streak(&code).await;
        if streak >= 3 {
            let reason = format!(
                "reconcile 재확인 {}회 연속 전부 실패 — 상태 불명, 수동 개입 필요",
                streak
            );
            error!("[reconcile] {}: {}", code, reason);
            // status 갱신 후 helper 로 manual 승격
            ...
            crate::strategy::live_runner::enter_manual_intervention_state(
                &rs,
                Some(&self.active_position_lock),
                &stop_flag,
                &stop_notify,
                self.event_logger.as_ref(),
                &code,
                "balance_reconcile_inconclusive_escalated",
                reason,
                serde_json::json!({
                    "runner_qty": runner_qty,
                    "kis_qty_first": initial_kis_qty,
                    "streak": streak,
                    "error": e,
                }),
            ).await;
            continue;
        }
        warn!(
            "[reconcile] {} 재확인 모든 attempt 실패 — inconclusive({}회), 다음 cycle 재시도: {}",
            code, streak, e
        );
        if let Some(ref el) = self.event_logger {
            el.log_event(
                &code,
                "position",
                "balance_reconcile_inconclusive",
                "warn",
                "재확인 모든 attempt 실패 — manual 전환 보류",
                serde_json::json!({
                    "runner_qty": runner_qty,
                    "kis_qty_first": initial_kis_qty,
                    "streak": streak,
                    "error": e,
                }),
            );
        }
        continue;
    }
};
```

**근거**: false manual stop 비용 > 1~2 cycle 지연 비용. 다만 무기한 유예는
위험하므로 `3회 연속 inconclusive` 를 상한으로 두고 그 이후는 "상태 불명"으로
manual 승격한다.

**구현 메모**:
- `StrategyManager` 내부 필드 추가:
  - `reconcile_inconclusive_streaks: Arc<RwLock<HashMap<String, u32>>>`
- helper 2개 추가:
  - `bump_reconcile_inconclusive_streak(code) -> u32`
  - `reset_reconcile_inconclusive_streak(code)`
- `confirm_reconcile_qty()` 가 `Ok(_)` 를 반환한 모든 경로에서 streak reset

### F2 — manual 스킵 시 active_position 동적 필드 재저장

**문제**: `Position` struct 의 동적 필드 (`stop_loss`, `best_price`,
`reached_1r`) 는 trailing/본전스탑 진행 시 메모리에서만 갱신. DB 의
`active_positions` 행은 entry 시점 (`live_runner.rs:2851`) 또는 TP 발주 시점
(`live_runner.rs:751`) 에만 저장. 즉 **trailing 진행 후 reconcile mismatch 발생 시
보존되는 DB 행은 stale**.

**해결: 새 DB write 경로를 만들지 않고 기존 `update_position_trailing()` 재사용**.

이번 후속 수정의 목적은 `stop_loss`, `best_price`, `reached_1r` 의 stale 문제를
닫는 것이다. TP 주문번호까지 건드리는 새 partial update 메서드는 현재 결함
범위를 넘어서는 과설계로 본다. 따라서 F2 는 **기존 3필드 update 경로 재사용**에
한정한다.

**`live_runner.rs` 신규 helper**:
```rust
/// 2026-04-17 F2: manual_intervention 스킵 분기 진입 시 active_positions
/// 동적 필드를 재저장. db_store 또는 current_position 이 없으면 no-op.
/// 실패 시 critical 이벤트만 기록 — 자동 청산 차단 정책은 그대로 유지.
async fn persist_current_position_for_manual_recovery(&self) {
    let Some(ref store) = self.db_store else { return };
    let pos = {
        let state = self.state.read().await;
        state.current_position.clone()
    };
    let Some(pos) = pos else { return };

    let result = store
        .update_position_trailing(
            self.stock_code.as_str(),
            pos.stop_loss,
            pos.reached_1r,
            pos.best_price,
        )
        .await;

    match result {
        Ok(_) => info!(
            "{}: manual 스킵 — active_position 동적 필드 재저장 (sl={}, best={}, r1={})",
            self.stock_name, pos.stop_loss, pos.best_price, pos.reached_1r
        ),
        Err(e) => {
            error!(
                "{}: manual 스킵 — active_position 재저장 실패: {e}",
                self.stock_name
            );
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "storage",
                    "active_position_persist_failed",
                    "critical",
                    &format!("manual_intervention 스킵 분기에서 active_position 재저장 실패: {e}"),
                    serde_json::json!({
                        "stop_loss": pos.stop_loss,
                        "best_price": pos.best_price,
                        "reached_1r": pos.reached_1r,
                        "error": e.to_string(),
                    }),
                );
            }
        }
    }
}
```

**호출 위치**:
- 메인 루프 manual 스킵 분기 (`live_runner.rs:1269-1287`) — `break` 직전
- `manage_position` manual 스킵 분기 (`live_runner.rs:1980-1995`) — `return Ok(None)` 직전

**중복 호출 회피**: 두 분기가 같은 사이클에 모두 발동하지 않음 (메인 루프가
`is_stopped()` 분기를 먼저 잡으면 manage_position 까지 안 감, 반대도 마찬가지).
설사 중복돼도 UPDATE 는 idempotent.

**값 동일 시 생략 안 함**: SQL UPDATE 한 번 비용 vs 비교 read 한 번 비용 +
race 위험. 매번 UPDATE 가 단순하고 안전. 1분 주기 reconcile 에서만 발생.

### F3 — manual 전환 공용 helper

**문제**: 두 callsite 가 정책을 분리 구현. invariant 갈라짐 + reconcile_once
는 shared `position_lock` 미처리.

**해결: free function helper `enter_manual_intervention_state` 추가**.

호출자별 차이 (statuses 갱신 등)는 호출자가 별도로 처리. 공통 부분(state
+ event_log + position_lock + stop_flag) 만 helper 가 책임.

**`live_runner.rs` 또는 별도 모듈에 helper 추가**:
```rust
/// 2026-04-17 F3: manual_intervention 상태 전환의 공용 진입점.
///
/// 정책:
/// 1. RunnerState 의 manual / degraded / phase / reason 일괄 설정
/// 2. shared position_lock 처리
///    - Free / Pending(_) / Held(code) -> Held(code)
///    - Held(other) -> **silent overwrite 금지**
///      기존 lock 을 유지하고 `manual_intervention_lock_conflict` 추가 기록
/// 3. event_log 에 호출자별 event_type (critical) 기록
/// 4. stop_flag 세팅 + notify
///
/// `LiveRunner::trigger_manual_intervention` 와 `StrategyManager::reconcile_once`
/// 의 manual 전환은 이 helper 를 통해 일관된 정책으로 처리한다. statuses
/// (UI 표시용) 갱신 등 호출자별 부가 처리는 호출자가 별도 수행.
pub(crate) async fn enter_manual_intervention_state(
    state: &Arc<RwLock<RunnerState>>,
    position_lock: Option<&Arc<RwLock<PositionLockState>>>,
    stop_flag: &Arc<AtomicBool>,
    stop_notify: &Arc<Notify>,
    event_logger: Option<&Arc<EventLogger>>,
    code: &str,
    event_type: &str,
    reason: String,
    metadata: serde_json::Value,
) {
    {
        let mut s = state.write().await;
        s.phase = "수동 개입 필요".to_string();
        s.manual_intervention_required = true;
        s.degraded = true;
        s.degraded_reason = Some(reason.clone());
    }
    let mut lock_conflict: Option<String> = None;
    if let Some(lock) = position_lock {
        let mut guard = lock.write().await;
        match &*guard {
            PositionLockState::Held(existing) if existing != code => {
                lock_conflict = Some(existing.clone());
            }
            _ => {
                *guard = PositionLockState::Held(code.to_string());
            }
        }
    }
    if let Some(el) = event_logger {
        el.log_event(
            code,
            "position",
            event_type,
            "critical",
            &reason,
            metadata,
        );
        if let Some(existing) = lock_conflict {
            el.log_event(
                code,
                "position",
                "manual_intervention_lock_conflict",
                "critical",
                "manual_intervention 진입 중 shared position_lock 충돌",
                serde_json::json!({
                    "requested_code": code,
                    "existing_lock_holder": existing,
                }),
            );
        }
    }
    stop_flag.store(true, Ordering::Relaxed);
    stop_notify.notify_waiters();
}
```

**적용**:
1. `LiveRunner::trigger_manual_intervention` (`live_runner.rs:903-934`):
   - 본문을 helper 호출로 교체
   - `ManualInterventionMode::KeepLock` 인자는 helper 가 항상 lock 처리하므로
     simplify (또는 mode enum 유지하되 `Free` variant 추가)
   - position_lock 인자는 `self.position_lock.as_ref()` 전달

2. `StrategyManager::reconcile_once` (`strategy.rs:947-984`):
   - 기존 state 직접 갱신 + event_log + stop_flag 코드를 helper 호출로 교체
   - position_lock 인자: `Some(&self.active_position_lock)` 전달
   - statuses 갱신은 reconcile_once 가 별도 처리 (UI 표시용, helper 책임 아님)

**리팩터 후 `reconcile_once` 의 mismatch 분기**:
```rust
let reason = format!("...");
error!("{}: {}", code, reason);

// statuses (UI) 갱신은 reconcile_once 책임
{
    let mut s = self.statuses.write().await;
    if let Some(status) = s.get_mut(&code) {
        status.manual_intervention_required = true;
        status.degraded = true;
        status.degraded_reason = Some(reason.clone());
        status.state = "수동 개입 필요".to_string();
        status.message = format!("⚠ {}", reason);
    }
}

// 공용 helper 호출 — state/lock/event/stop 일괄 처리
    crate::strategy::live_runner::enter_manual_intervention_state(
        &rs,
        Some(&self.active_position_lock),
        &stop_flag,
        &stop_notify,
        self.event_logger.as_ref(),
        &code,
        "balance_reconcile_mismatch",
        reason,
        serde_json::json!({
            "runner_qty": runner_qty,
            "kis_qty": confirmed_kis_qty,
            "kis_qty_first": initial_kis_qty,
    }),
).await;
```

**부수 효과**:
- reconcile mismatch 시 다른 종목 진입이 자동 차단됨 (position_lock = Held)
- 기존 `trigger_manual_intervention(KeepLock)` 호출 경로 (safe_orphan_cleanup
  등) 도 동일 helper 사용 → 정책 일원화
- `ManualInterventionMode` enum 의 의미 재검토 필요. 현재 `KeepLock` 만 존재
  이지만 helper 가 기본적으로 lock 을 잡으므로 enum 자체가 무의미에 가깝다.
  단, lock conflict 정책을 helper 가 흡수하므로 **enum 제거는 선택사항**으로 둔다.
- `Held(other)` 충돌은 silent overwrite 하지 않는다. 이 경우에도 기존 lock 이
  이미 신규 진입을 막고 있으므로, 우선 증거 보존과 경고 로그가 더 중요하다.

### F4 — 회귀 테스트 보강

**T1: 메인 루프 stop+manual+position=Some → close 미호출**

메인 루프는 `pub async fn run()` 같은 큰 함수의 일부라 직접 호출 어려움.
**대안 두 가지**:

- **T1a**: `manage_position` 통합 테스트를 강화 — `current_position=Some(...)`
  로 채우고 manual=true vs manual=false 매트릭스. dummy http 클라이언트가 close
  시도 시 Err 반환하는 점을 활용:
  - manual=true → `Ok(None)` 반환, dummy http 호출 0회
  - manual=false → close 시도 → dummy http Err → manage_position `Err` 반환
  - 두 케이스 비교로 "manual 분기가 close 를 막았다" 강한 증거

- **T1b (필수)**: 메인 루프의 stop 분기 처리 로직을 helper 함수로 분리 +
  단위테스트. 실제 사고는 메인 루프 경로에서 발생했으므로, T1a 만으로는
  충분하지 않다. 최소 acceptance 는 **T1a + T1b 둘 다**다.

**T2: confirm_reconcile_qty_with 신규 동작 케이스 4개**
- error → success(일치) → 즉시 `Ok(expected)`, 호출 2회
- error → mismatch → mismatch → `Ok(last_qty)`, 호출 3회 (일부 성공 → 마지막 값)
- error → error → error → `Err("재확인 모든 attempt 실패: ...")`, 호출 3회
- success(불일치) → success(일치) → 즉시 `Ok(expected)`, 호출 2회 (기존 케이스 유지 확인)

**T3: reconcile_once inconclusive 분기**

reconcile_once 직접 호출은 StrategyManager 전체 mock 필요 → 어려움.
**대안**: reconcile_once 의 결정부를 별도 internal method 로 추출
(`handle_reconcile_decision(...)`) 하여 아래 2개를 단위테스트한다.

- `inconclusive_streak = 1, 2` -> manual 전환 안 함, warn 이벤트
- `inconclusive_streak = 3` -> `balance_reconcile_inconclusive_escalated` 로 manual 전환

이건 F1.1의 핵심 정책이므로 **hotfix 범위 안에 포함**한다.

**T4: F2 active_position 재저장 검증**

PostgresStore 의존이라 단위테스트 어려움. 다만 F2 는 기존
`update_position_trailing()` 재사용으로 범위를 줄였으므로, 최소한 아래 둘 중
하나는 필요하다.

- helper 를 작은 trait 경계 뒤로 빼서 mock 호출 횟수/인자 검증
- 실 DB 통합 테스트 1건으로 `stop_loss/best_price/reached_1r` 갱신 확인

완전한 mock 인프라까지는 hotfix 범위 밖이어도 되지만, **재저장 호출 자체의 검증은
필수**로 본다.

## 검증 체크리스트 (배포 전)

- [ ] `cargo fmt -- --check` 통과
- [ ] `cargo clippy --lib --tests` 신규 경고 0
- [ ] `cargo test --lib` 전체 통과 (현재 166 + 신규 ~6 = 172 예상)
- [ ] **F1 신규 테스트 4건** (위 T2)
- [ ] **F1.1 streak 테스트**: inconclusive 1/2회는 보류, 3회째는 manual 승격
- [ ] **F4 T1a 매트릭스 테스트** (manage_position with current_position=Some,
      manual=true / manual=false)
- [ ] **F4 T1b 메인 루프 stop helper 테스트**: 실제 사고 경로에서 close 미호출
- [ ] **F3 적용 후 기존 manual_intervention_tests 전부 그대로 통과** (회귀 0)
- [ ] **F3 lock conflict 테스트**: 기존 lock 이 `Held(other)` 일 때 silent overwrite
      하지 않고 `manual_intervention_lock_conflict` 이벤트를 남김
- [ ] **F2 persistence 테스트 1건**: manual 스킵 시 trailing 필드 재저장 호출 확인
- [ ] `backtest 122630,114800 --days 5 --multi-stage --dual-locked` 결과 핫픽스
      전후 거래수/entry/exit/pnl 완전 동일 (라이브 전용 경로만 수정)
- [ ] `replay 122630 --date 2026-04-16` parity/passive 첫 거래 +0.48% 유지
- [ ] **F3 helper 호출 후 shared `position_lock` 이 `Held(code)` 로 전이**되는지
      reconcile mismatch 통합 테스트 (mock manager 또는 직접 helper 호출 단위 테스트)

## 영향 받는 파일

| 파일 | 변경 내용 |
|---|---|
| `src/presentation/routes/strategy.rs` | F1 `confirm_reconcile_qty_with` 본문 교체. F1.1 inconclusive streak/승격 로직 추가. F3 reconcile_once mismatch 분기를 공용 helper 호출로 교체 |
| `src/strategy/live_runner.rs` | F2 `persist_current_position_for_manual_recovery` helper 추가, 메인 루프(1269) 와 manage_position(1980) manual 스킵 분기에서 호출. F3 `enter_manual_intervention_state` 공용 helper 추가, `trigger_manual_intervention` 본문을 helper 호출로 교체. F4 manage_position + 메인 루프 stop helper 테스트 강화 |

## 롤백

증상 발생 시:
1. `/api/v1/strategy/stop` 또는 Ctrl+C
2. git revert 본 커밋 → cargo build --release
3. DB 스키마 변경 없음 (F2 는 컬럼 추가 없이 UPDATE 만)
4. 재시작 — `check_and_restore_position` 으로 복구

## 단계 배포 (Day+0 단일)

F1~F4 한 PR 로 통합. F1/F2/F3/F4 각 커밋 분리 권장 (bisect 용이성).

순서:
1. **F1 + F4-T2**: confirm_reconcile_qty_with 강화 + 단위테스트 4건
2. **F1.1 + T3**: inconclusive streak helper/결정부 추가 + 3회 연속 실패 승격 테스트
3. **F2 + helper**: 기존 `update_position_trailing` 재사용 + LiveRunner helper + 호출 추가
4. **F3 + helper**: enter_manual_intervention_state + trigger_manual_intervention 리팩터 + reconcile_once 적용 + position_lock 전이/충돌 검증 테스트
5. **F4-T1a/T1b**: manage_position 매트릭스 + 메인 루프 stop helper 테스트
6. `cargo fmt && cargo clippy && cargo test --lib` 통과
7. backtest/replay 회귀 검증
8. release 빌드

## v3 적용 후 종합 평가 (사전 예측)

| 평가 축 | v2 (P0+P1만) | v3 (F1~F4 추가 후) |
|---|---|---|
| 자동 청산 → 슬리피지 손실 차단 | ✅ 완전 | ✅ 완전 (변동 없음) |
| 재시작 복구 정합성 | ⚠ trailing 후 stale | ✅ 동적 필드 보존 |
| false manual stop 빈도 | ⚠ KIS API 5xx 1번에도 발동 | ✅ inconclusive 로 보류 → 다음 cycle 재확인 |
| 정책 일원화 | ⚠ 두 callsite 분리 | ✅ 공용 helper |
| dual-locked 다른 종목 차단 | ⚠ reconcile mismatch 시 누락 | ✅ position_lock = Held |
| 회귀 감시 | ⚠ helper + None 케이스만 | ✅ Some 매트릭스 + reconcile error 케이스 |

위 6개 축이 모두 ✅ 가 되면 "기본 reconcile 오판 손실 문제는 해결됐다" 라고
말할 수 있다.

## v3 이후 follow-up (별도 PR)

- **P3 (v2 follow-up F1)**: `trades.environment` 하드코딩 제거 — 손실 무관, 포렌식 위생
- **P4 (v2 follow-up F2)**: cancel/fill race 후 60s reconcile grace — 알람 노이즈 감소
- **P5 (v2 follow-up F2)**: cancel 전 inquire-psbl-rvsecncl 사전 조회 — race 사전 차단 (효과 작음)
- **F2 추가**: PostgresStore mock trait 추출 + `persist_current_position_for_manual_recovery` 통합 테스트
- **T3**: reconcile_once mismatch 분기 helper 분리 + 단위테스트
- **T1b**: 메인 루프 stop 분기 helper 분리 + 단위테스트
