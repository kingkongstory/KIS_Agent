# 2026-04-17 reconcile 후속 핫픽스 마감 fixups (v3+ 잔존 결함 닫기)

> v3+ (`2026-04-17-reconcile-hotfix-followup.md` 기반) 적용 완료 후 자체
> 비판 + 외부 리뷰에서 드러난 5가지 잔존 결함을 닫는 마감 PR. 이 fixups
> 적용 후에 비로소 "기본 reconcile 오판 손실 문제 해결" 마감으로 서명 가능.

## Context

v3+ 적용 후 평가에서 6축 중 5개 ✅, 1개 ⚠ (정책 일원화 — Held(other) 보호 누락).
추가로 외부 리뷰가 두 건의 새 결함 식별:

| # | 출처 | 결함 | 영향 |
|---|---|---|---|
| **A** | 자체 | F3 `enter_manual_intervention_state` 가 `Held(other)` silent overwrite. 계획서 line 264-266 명세 위반 | dual-locked 운용에서 다른 종목 lock 사라짐 → close_position_market 후 lock=Free 전이로 unresolved manual position 잔존 중 다른 종목 진입 재허용. **F3 invariant 미닫힘** |
| **B** | 자체 | F3 충돌 단위테스트 부재 | 정책 회귀 감시 0 |
| **C** | 자체 | F1.1 streak 단위테스트 부재 (계획서 line 411-420 "hotfix 범위 안" 명시) | 임계값 / bump/reset 회귀 감시 0 |
| **D** | 외부 #2 | reconcile_once 빠른 스킵 분기 (`runner_qty == initial_kis_qty` line 952) 에서 streak reset 누락. confirm_reconcile_qty 가 호출조차 안 되는 정상 일치 cycle 에서 streak 가 누적 상태로 남음 | "연속 inconclusive" 가 아니라 "누적 inconclusive" 로 동작 → 예: inconclusive 2 → match 1 → inconclusive 1 → 3회로 승격되어 false escalation |
| **E** | 외부 #3 | `PostgresStore::update_position_trailing()` 가 `rows_affected()` 검사 안 함. 행 자체가 없어도 `Ok(())` 반환 | F2 가 "stale row" 문제는 줄였지만 "행 부재" 시나리오에서 silent no-op. 복구 정합성 부분만 보장 |
| **F** | 자체 | escalated 분기에서 streak reset 누락 | 운영자 manual 해제 후 무재시작 시 streak 그대로 남아 다음 inconclusive 1회에 즉시 false escalation |
| **G** | 자체 | F2 persistence 호출 검증 테스트 부재 (계획서 line 431-432 "필수" 명시) | P0 분기 리팩터 시 호출 빠뜨려도 못 잡음 |

## 우선순위

1. **A + B**: F3 invariant 닫기 (dual-locked 안전성 직결)
2. **D + F**: streak semantics 정상화 ("연속" 의미 회복)
3. **E**: update_position_trailing rows_affected 검증
4. **C + G**: 회귀 단위테스트

## 구현 세부

### A — F3 `enter_manual_intervention_state` 의 Held(other) 보호

**파일**: `src/strategy/live_runner.rs:4262-4264`

**현재**:
```rust
if let Some(lock) = position_lock {
    *lock.write().await = PositionLockState::Held(code.to_string());
}
```

**수정 후**:
```rust
let mut lock_conflict: Option<String> = None;
if let Some(lock) = position_lock {
    let mut guard = lock.write().await;
    match &*guard {
        PositionLockState::Held(existing) if existing != code => {
            // 다른 종목이 이미 Held — 덮어쓰지 말고 보존 + 충돌 기록.
            // 이 종목의 manual 은 자체 manual_intervention_required 플래그로 차단되므로
            // shared lock 을 굳이 바꿀 필요 없다. 다른 종목은 자기 청산 사이클로
            // lock 을 Free 로 풀 수 있는데, 그 시점에도 이 종목은 manual 로 잡혀
            // 신규 진입을 안 함 (start_runner 의 manual 체크).
            lock_conflict = Some(existing.clone());
        }
        _ => {
            *guard = PositionLockState::Held(code.to_string());
        }
    }
}
if let Some(el) = event_logger {
    el.log_event(code, "position", event_type, "critical", &reason, metadata);
    if let Some(existing) = lock_conflict {
        el.log_event(
            code,
            "position",
            "manual_intervention_lock_conflict",
            "critical",
            "manual_intervention 진입 중 shared position_lock 충돌 — 기존 lock 유지",
            serde_json::json!({
                "requested_code": code,
                "existing_lock_holder": existing,
            }),
        );
    }
}
```

**중요**: helper 의 `event_logger` 호출 블록을 `lock_conflict` 처리 후로 이동
필요 (현재 코드 상 lock 처리가 event 호출 전에 끝나야 함). 이미 그 순서임.

### B — F3 충돌 단위테스트

**파일**: `src/strategy/live_runner.rs` (manual_intervention_tests 모듈)

**케이스**:
1. `Held("114800")` 상태에서 `enter_manual_intervention_state(code="122630")` 호출
   → lock 은 `Held("114800")` 그대로 유지, `manual_intervention_lock_conflict`
     event 1건 critical, 메인 manual_intervention_required event 도 1건 critical (총 2건)
2. `Free` 상태에서 호출 → `Held("122630")` 로 전이, conflict 이벤트 0건
3. `Pending(other)` 상태에서 호출 → `Held("122630")` 로 전이 (Pending 은 보호 대상
   아님, 기존 `trigger_manual_intervention_overwrites_prior_lock_with_self_held`
   테스트 회귀 보존), conflict 이벤트 0건

mock event_logger 가 필요. 기존 `build_runner` 는 event_logger=None 이라 conflict
이벤트 검증 불가. 두 가지 접근:
- 옵션 1: helper 를 직접 호출하는 단위테스트 (LiveRunner 우회)에서 in-memory mock
  EventLogger trait 또는 카운터 사용
- 옵션 2: 기존 LiveRunner 인프라 활용하되 event_logger 없는 케이스만 검증
  (lock 상태만 검증 — conflict 이벤트 검증 포기)

**판단**: 옵션 2 — 단순. lock 상태가 invariant 의 핵심이고 이벤트는 보조 기록.

### D + F — streak semantics 정상화

**파일**: `src/presentation/routes/strategy.rs:937-1031` (reconcile_once)

#### D: 빠른 스킵 분기에서 reset

**현재 (line 951-954)**:
```rust
let initial_kis_qty: u64 = holdings.get(&code).map(|(_, q)| *q).unwrap_or(0);
if runner_qty == initial_kis_qty {
    continue;
}
```

**수정 후**:
```rust
let initial_kis_qty: u64 = holdings.get(&code).map(|(_, q)| *q).unwrap_or(0);
if runner_qty == initial_kis_qty {
    // 1차 fetch 가 이미 일치 — 정상 cycle. streak 초기화.
    self.reset_reconcile_inconclusive_streak(&code).await;
    continue;
}
```

#### F: escalated 분기에서도 reset

**현재 (escalated 분기, line 972-1008)**: streak 카운터 그대로 두고 enter_manual.

**수정 후**: enter_manual 호출 직전에 reset 추가:
```rust
if streak >= 3 {
    // ... statuses 갱신 ...
    // 승격 후 streak 카운터는 의미 없음 — 운영자가 manual 해제 후
    // 재시작 없이 운영을 재개하더라도 false escalation 누적이 안 되도록 reset.
    self.reset_reconcile_inconclusive_streak(&code).await;
    crate::strategy::live_runner::enter_manual_intervention_state(...).await;
    continue;
}
```

**효과**: streak 가 진짜 "연속 inconclusive" 의미로 작동. 정상 일치/recovered/
승격 모두 reset 경로.

### E — `update_position_trailing` rows_affected 검증

**파일**: `src/infrastructure/cache/postgres_store.rs:813-832`

**현재**:
```rust
pub async fn update_position_trailing(...) -> Result<(), KisError> {
    sqlx::query("UPDATE active_positions SET ... WHERE stock_code = $1")
        ...
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("...: {e}")))?;
    Ok(())
}
```

**수정 후**:
```rust
pub async fn update_position_trailing(...) -> Result<u64, KisError> {
    let result = sqlx::query("UPDATE active_positions SET ... WHERE stock_code = $1")
        ...
        .execute(&self.pool)
        .await
        .map_err(|e| KisError::Internal(format!("활성 포지션 트레일링 갱신 실패: {e}")))?;
    Ok(result.rows_affected())
}
```

**호출자 영향**:
1. `live_runner.rs:2155` (정상 trailing 갱신, fire-and-forget spawn) — 시그니처
   변경에 무영향 (`let _ = ...`)
2. `live_runner.rs:946` (F2 persist helper) — 결과 매칭 강화:
   ```rust
   match store.update_position_trailing(...).await {
       Ok(rows) if rows > 0 => info!(...),
       Ok(_) => {
           // 행 자체가 없음 — entry 시점 save_active_position 실패 또는
           // 외부 삭제. F2 가 의도한 "동적 필드 보존" 자체가 불가능.
           error!(
               "{}: manual 스킵 — active_position 행 자체 없음 (rows=0)",
               self.stock_name
           );
           if let Some(ref el) = self.event_logger {
               el.log_event(
                   self.stock_code.as_str(),
                   "storage",
                   "active_position_missing",
                   "critical",
                   "manual 스킵 시 active_positions 행 부재 — 재시작 복구 불가",
                   serde_json::json!({
                       "stop_loss": sl,
                       "reached_1r": r1r,
                       "best_price": bp,
                   }),
               );
           }
       }
       Err(e) => { /* 기존 critical 그대로 */ }
   }
   ```

신규 이벤트 타입: `active_position_missing` (critical).

### C — F1.1 streak 단위테스트

**파일**: `src/presentation/routes/strategy.rs` (별도 모듈 `streak_tests`)

helper 자체는 `&self` 메서드라 StrategyManager 인스턴스 필요. `StrategyManager::new()`
는 dummy 의존성 0개 — 그대로 사용 가능.

**케이스**:
1. 신규 종목 → bump 1회 → 결과 1, HashMap 에 ("122630", 1)
2. 같은 종목 bump 3회 → 결과 1/2/3 순차 반환
3. bump 후 reset → HashMap 에서 키 제거, 다음 bump 는 1로 시작
4. 다른 종목 bump 들이 서로 격리 (122630 의 bump 가 114800 streak 에 영향 안 줌)

### G — F2 persistence 호출 검증

**파일**: `src/strategy/live_runner.rs` (manual_intervention_tests 모듈)

`build_runner` 는 db_store=None 이라 persist 호출 시 no-op. 호출 자체 검증을
위해서는 db_store mock 이 필요하지만 PostgresStore mock trait 추출은 hotfix 범위
밖. 차선:

- **G-1**: persist helper 가 db_store=None 이면 no-op 으로 빠르게 반환하는지
  유닛 검증 (간접). 즉 helper 가 panic 없이 호출되는지 확인.
- **G-2**: `manage_position_with_filled_position_skips_close_when_manual_active`
  테스트의 doc comment 에 "F2 persist 가 호출되지만 db_store=None 이라 no-op 이며
  panic 없이 통과하는 것이 검증 일부" 명시.

이 hotfix 범위에서는 G-1/G-2 만 추가. **trait 추출 + 호출 횟수 mock 검증은
follow-up** (계획서 line 502 도 동일 범주로 분류).

## 검증 체크리스트

- [ ] `cargo fmt -- --check`
- [ ] `cargo clippy --lib --tests` 신규 경고 0
- [ ] `cargo test --lib` 전부 통과 (175 + 신규 ~7 = 182 예상)
- [ ] **A 로 lock 정책 변경 후 기존 manual_intervention_tests 4건 회귀 0**
  - 특히 `trigger_manual_intervention_overwrites_prior_lock_with_self_held` 는
    Pending(other) 케이스라 통과해야 함
- [ ] **B 신규 테스트 3건** (Held(other) / Free / Pending(other))
- [ ] **C 신규 테스트 4건** (streak bump/reset/격리)
- [ ] **D + F 동작 검증**: reconcile_once 흐름 통합 테스트는 어렵지만 코드 리뷰로 보장
- [ ] **E 동작 검증**: `update_position_trailing` 시그니처 변경 호환성 (호출자 2곳)
- [ ] `backtest 122630,114800 --days 5 --multi-stage --dual-locked` 결과 동일
- [ ] `replay 122630 --date 2026-04-16` parity/passive 첫 거래 +0.48% 유지
- [ ] release 빌드 성공

## 영향 받는 파일

| 파일 | 변경 |
|---|---|
| `src/strategy/live_runner.rs` | A: enter_manual_intervention_state 에 Held(other) 분기 + lock_conflict 이벤트. E: persist helper 에서 rows=0 분기. B/G 테스트 추가 |
| `src/presentation/routes/strategy.rs` | D: 빠른 스킵 분기 streak reset. F: escalated 분기 streak reset. C 테스트 추가 |
| `src/infrastructure/cache/postgres_store.rs` | E: update_position_trailing 시그니처 → `Result<u64, KisError>` (rows_affected 반환) |

## 단계별 커밋

1. **A + B** — F3 invariant + 단위테스트
2. **D + F** — streak semantics 정상화
3. **E** — update_position_trailing rows_affected
4. **C + G** — 단위테스트 보강
5. cargo fmt / clippy / test
6. backtest / replay 회귀
7. release 빌드

## 마감 평가 매트릭스

| 평가 축 | v3+ | v3+ + fixups |
|---|---|---|
| 자동 청산 슬리피지 손실 차단 | ✅ | ✅ |
| 재시작 복구 정합성 | ⚠ 행 부재 미감지 | ✅ active_position_missing critical |
| false manual stop 빈도 | ⚠ "누적" streak | ✅ 진짜 "연속" semantics |
| 정책 일원화 | ⚠ Held(other) overwrite | ✅ 충돌 보존 + 이벤트 |
| dual-locked 다른 종목 차단 | ⚠ silent overwrite | ✅ invariant 닫힘 |
| 핵심 회로 회귀 감시 | ✅ | ✅ + streak/conflict 단위테스트 |

6축 모두 ✅ → "기본 reconcile 오판 손실 문제 해결" 서명 가능.

## 신규/변경 이벤트 타입

- `manual_intervention_lock_conflict` (critical, 신규) — Held(other) 충돌 시
- `active_position_missing` (critical, 신규) — F2 persist 시 행 부재

---

# v2 추가 (마감 다듬기)

> v3 fixups 적용 후 외부 추가 리뷰에서 1건 medium 결함 + 자체 마감 다듬기 4건
> 식별. 본 섹션은 hotfix 완전 마감을 위한 추가 적용분.

## 추가 결함

| # | 출처 | 결함 | 영향 |
|---|---|---|---|
| **H** | 외부 #1 (Medium) | fixups A 가 `Held(other)` silent overwrite 만 막았지만, 기존 holder 가 정상 청산하면 `live_runner.rs:3206/2176/1568/1276` 가 lock 을 무조건 `Free` 로 푼다. 그 후 `poll_and_enter:1843` 가 `Free` 만 보고 다시 진입 가능 | manual 종목의 unresolved live position 이 남아있는 동안 다른 종목 진입 차단이라는 강한 안전 목표 미달 |
| **I** | 자체 (Trivial) | F3 매트릭스에 `Held(self)` 4번째 케이스 누락 | 분기 4개 중 3개만 못 박음 |
| **J** | 자체 (Trivial) | G 테스트 doc 오류 — "두 번째 early return 분기로 빠짐" 이라 적었지만 두 호출 모두 첫 번째(`db_store=None`) 분기로 빠짐 | 정직성 |
| **K** | 자체 (Medium) | reconcile_once streak 분기 자체(빠른 스킵 reset / Err bump / 3회 escalation) 통합 단위테스트 부재. C 는 streak helper(bump/reset) 만 검증 | D+F 정책 회귀 감시 비어있음 |

## 우선순위

1. **H** — dual-locked 진정한 안전성 닫기 (변경 범위 큼, 신중)
2. **K** — reconcile decision helper 추출 + 단위테스트
3. **I + J** — trivial 다듬기

## H — `PositionLockState::ManualHeld(code)` variant 신규

### 의도

manual_intervention 으로 보유 중인 종목의 lock 을 일반 `Held` 와 구분. close
경로(line 3206 등)는 자기 자신 `Held(self)` 만 `Free` 로 풀고, `ManualHeld(_)`
는 절대 건드리지 않는다. `poll_and_enter` 는 `ManualHeld(_)` 면 다른 종목이라도
신규 진입 차단.

### enum 변경

```rust
pub(crate) enum PositionLockState {
    Free,
    Pending { code: String, preempted: bool },
    Held(String),
    /// 2026-04-17 v3 fixups H: manual_intervention 으로 보유 중인 종목 표시.
    /// `enter_manual_intervention_state` 가 설정. close 경로는 무시 (자기 lock 만 풀음).
    /// `poll_and_enter` 는 다른 종목 ManualHeld 일 때 진입 차단.
    ManualHeld(String),
}
```

### `enter_manual_intervention_state` 변경 (`live_runner.rs:4262`)

```rust
match &*guard {
    PositionLockState::Held(existing) if existing != code => {
        lock_conflict = Some(existing.clone());
    }
    PositionLockState::ManualHeld(existing) if existing != code => {
        // 다른 종목이 이미 manual — 그대로 유지 + conflict 기록
        lock_conflict = Some(existing.clone());
    }
    _ => {
        // Free / Pending(any) / Held(self) / ManualHeld(self)
        *guard = PositionLockState::ManualHeld(code.to_string());
    }
}
```

### close 경로 변경 — 자기 holder 만 Free

영향 사이트 4곳: `live_runner.rs:1276`, `1568`, `2176`, `3206`. 모두 무조건
`*lock.write().await = PositionLockState::Free` 패턴. 다음 helper 로 일원화:

```rust
/// 2026-04-17 v3 fixups H: 자기 종목이 holder 인 경우에만 lock 을 Free 로 푼다.
/// 다른 종목이 Held / ManualHeld 로 보유 중이면 그대로 유지 (fixups A 의
/// Held(other) 보존과 같은 정책). lock 이 이미 Free 거나 다른 종목 Pending 인
/// 경우도 건드리지 않는다.
async fn release_lock_if_self_held(
    lock: &Arc<RwLock<PositionLockState>>,
    code: &str,
) {
    let mut guard = lock.write().await;
    match &*guard {
        PositionLockState::Held(c) if c == code => {
            *guard = PositionLockState::Free;
        }
        PositionLockState::Pending { code: c, .. } if c == code => {
            *guard = PositionLockState::Free;
        }
        _ => {
            // ManualHeld(any) / Held(other) / Pending(other) / Free / ManualHeld(self)
            // 모두 건드리지 않는다.
        }
    }
}
```

4곳 모두 `release_lock_if_self_held(lock, self.stock_code.as_str()).await;` 로 교체.

기존 `live_runner.rs:671-678` (run init 정리) 는 이미 self holder 만 푸는 패턴.
ManualHeld(self) 도 정리 대상에 포함하기 위해 패턴 보강 (start_runner 가 manual
종목을 거부하므로 도달 시 운영자가 manual 해제한 상태):

```rust
match &*guard {
    PositionLockState::Held(c)
    | PositionLockState::Pending { code: c, .. }
    | PositionLockState::ManualHeld(c)
        if c == self.stock_code.as_str() =>
    {
        *guard = PositionLockState::Free;
    }
    _ => {}
}
```

### `poll_and_enter` 진입 가드 변경 (`live_runner.rs:1843-1880`)

기존 match 에 `ManualHeld` arm 추가:

```rust
match &*guard {
    PositionLockState::Held(code) => { /* 기존 */ }
    PositionLockState::ManualHeld(code) => {
        debug!(
            "{}: 진입 차단 — {} 가 manual 보유 중 (unresolved live position)",
            self.stock_name, code
        );
        return Ok(false);
    }
    PositionLockState::Pending { code, preempted } => { /* 기존 */ }
    PositionLockState::Free => { /* 기존 */ }
}
```

### 신규 단위테스트

- `enter_manual_state_preserves_held_self_noop` (I 도 포함): Held(self) 상태에서
  enter_manual → ManualHeld(self) 로 전이 (또는 보존, 후술)
- `enter_manual_state_preserves_manual_held_other`: ManualHeld(other) 보존 + conflict
- `release_lock_only_when_self_holder`: Held(other)/ManualHeld(other)/Free 시 no-op
- `poll_and_enter_blocks_when_manual_held_other`: 진입 가드가 ManualHeld(other)
  를 차단 (helper 직접 호출 어려우면 메인 루프 분기 helper 처럼 추출 권장)

## K — `reconcile_once` decision helper 추출 + 단위테스트

### Helper 시그니처

```rust
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReconcileDecision {
    /// runner_qty == initial_kis_qty — 정상 cycle, streak reset 후 skip
    AlreadyMatched,
    /// confirm Ok 일치 회복 — recovered 이벤트 + skip
    Recovered { confirmed_qty: u64 },
    /// confirm Ok mismatch — manual 전환 (balance_reconcile_mismatch)
    Mismatch { confirmed_qty: u64 },
    /// confirm Err + streak < 3 — inconclusive warn + skip
    Inconclusive { streak: u32 },
    /// confirm Err + streak >= 3 — escalated manual 전환
    InconclusiveEscalated { streak: u32 },
}

pub(crate) fn classify_reconcile_outcome(
    runner_qty: u64,
    initial_kis_qty: u64,
    confirm_result: Result<u64, ()>,
    streak_after_bump: u32,  // Err 인 경우만 의미 있음
) -> ReconcileDecision { ... }
```

reconcile_once 가 이 helper 를 호출하고 결과별 처리를 진행. helper 자체는 순수
결정 로직만 담당.

### 단위테스트 5건

- `runner_matches_initial_returns_already_matched`
- `confirm_ok_match_returns_recovered`
- `confirm_ok_mismatch_returns_mismatch`
- `confirm_err_streak_below_threshold_returns_inconclusive`
- `confirm_err_streak_at_threshold_returns_escalated`

### reconcile_once 리팩터

기존 흐름을 helper 호출 + match 결과 처리로 재구성. statuses/event/enter_manual 호출은
호출자가 처리. helper 는 결정만.

## I + J — Trivial 다듬기

- I: F3 매트릭스에 Held(self) 케이스 1건 추가 (no-op 확인)
- J: G 테스트 doc 정정 — "db_store=None 분기만 검증한다" 명시

## 검증 체크리스트 (v2 추가)

- [ ] H 신규 enum variant 컴파일 통과 (모든 match 사이트 exhaustiveness 충족)
- [ ] H release_lock_if_self_held helper 4곳 적용 + 기존 동작 동일성 (다른 종목 lock 보존)
- [ ] H poll_and_enter ManualHeld 차단 동작
- [ ] H 신규 4건 테스트 통과
- [ ] K decision helper 5건 테스트 통과
- [ ] I+J trivial 다듬기
- [ ] 기존 manual_intervention_tests 7건 회귀 0
- [ ] backtest/replay 회귀 0

## 단계별 커밋

1. **I + J** — Trivial 다듬기 (5분)
2. **K** — reconcile decision helper + 5건 테스트
3. **H** — enum ManualHeld + release helper + 사이트 4곳 + poll_and_enter + 4건 테스트
4. cargo fmt/clippy/test
5. backtest/replay 회귀
6. release 빌드
