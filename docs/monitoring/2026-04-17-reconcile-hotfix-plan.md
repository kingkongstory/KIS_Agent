# 2026-04-17 reconcile-mismatch 자동 청산 사고 핫픽스 계획 (v2)

> v1(2026-04-17 작성)에 대한 리뷰 반영본. Day+0 범위를 P0/P1로 좁히고 P2~P5는 follow-up으로 분리.

## Context

### 사고 요약 (2026-04-16 122630, paper burn-in 1일차)

- 10:03:38 95주 지정가 정상 체결 (entry=101,580)
- 10:04:19 reconcile에서 KIS `inquire-balance`가 0주 반환 (체결 41초 후에도 미반영)
- 2초 후 `confirm_reconcile_qty` 재확인도 0주 → `balance_reconcile_mismatch(critical)` + `stop_flag=true`
- 다음 러너 메인 루프 iteration에서 `is_stopped()` 분기가 `close_position_market(ExitReason::EndOfDay)` 실행
- 시장가 매도 95주 @101,410 — 슬리피지 170원/주 = **-0.17% (-16,150원)**

### 결정적 추가 근거 — 강제 청산 미발동 시 +0.48%

2026-04-17 `cargo run -- replay 122630 --date 2026-04-16` (parity/passive) 결과:
- 동일 신호의 첫 거래: **entry=101,580, exit=102,066, +0.48%**
- 즉 reconcile 자동 청산만 없었다면 **+0.48%로 정상 익절**됐을 거래.
- 실제 사고 손실 -0.17% + 놓친 익절 +0.48% = **약 0.65% 갭**이 강제 청산 한 번의 비용.

### 근본 결함

시스템이 정상 체결한 95주를 KIS 잔고 API의 일시 지연을 "숨은 포지션"으로 오판해 **스스로 시장가로 강제 청산**.
paper/real 무관하게 KIS API 지연 특성상 실전 재현 가능성이 합리적 경계 가정으로 성립.

### 목표 (Day+0)

- reconcile mismatch가 critical 알람으로 찍히되 **자동 청산 경로는 끊는다**.
- 일시 지연을 흡수하기 위해 재확인을 강화하되, 회복이 빠를 때는 즉시 통과시켜 진짜 hidden position 감지 지연을 최소화.
- 그 외 (포렌식 위생, 알람 노이즈 감소, race 사전 차단)는 follow-up으로 분리해 hotfix 검증 범위를 좁힌다.

## Day+0 hotfix 범위 (P0 + P1만)

| # | 항목 | 파일 | 핵심 |
|---|---|---|---|
| **P0** | `is_stopped()` 시 자동 청산 두 경로 모두 manual_intervention 체크 | `src/strategy/live_runner.rs:1252-1264` (메인 루프) **및** `src/strategy/live_runner.rs:1938-1943` (manage_position) | manual_intervention_required=true이면 청산 스킵, 포지션 보존 후 러너 종료 |
| **P1** | `confirm_reconcile_qty` — 5초 간격, 최대 3회, **첫 일치 시 조기 종료** | `src/presentation/routes/strategy.rs:446-454`, `src/config.rs` | env(`KIS_RECONCILE_CONFIRM_INTERVAL`, `KIS_RECONCILE_CONFIRM_ATTEMPTS`) 도입 |

## 구현 세부

### P0 — 자동 청산 두 경로 모두 차단

**경로 #1: 메인 루프 (`live_runner.rs:1252-1264`)** — 이게 reconcile 사고에서 실제로 탔던 경로

현재:
```rust
loop {
    if self.is_stopped() {
        info!("{}: 외부 중지 요청", self.stock_name);
        let state = self.state.read().await;
        if state.current_position.is_some() {
            drop(state);
            if let Ok(result) = self.close_position_market(ExitReason::EndOfDay).await {
                self.save_trade_to_db(&result).await;
                all_trades.push(result);
            }
        }
        break;
    }
    ...
}
```

수정:
```rust
loop {
    if self.is_stopped() {
        let state = self.state.read().await;
        let is_manual = state.manual_intervention_required;
        let has_position = state.current_position.is_some();
        drop(state);

        if is_manual && has_position {
            warn!(
                "{}: manual_intervention 활성 — 자동 청산 스킵, 포지션 DB 보존",
                self.stock_name
            );
            self.persist_active_position_for_recovery().await;  // 기존 save_active_position 재사용
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "position",
                    "auto_close_skipped",
                    "warn",
                    "manual_intervention 활성 — 운영자 수동 처리 대기",
                    serde_json::json!({"path": "main_loop"}),
                );
            }
            break;
        }

        info!("{}: 외부 중지 요청", self.stock_name);
        if has_position {
            if let Ok(result) = self.close_position_market(ExitReason::EndOfDay).await {
                self.save_trade_to_db(&result).await;
                all_trades.push(result);
            }
        }
        break;
    }
    ...
}
```

**경로 #2: manage_position (`live_runner.rs:1938-1943`)** — 동일 패턴 보호

현재:
```rust
async fn manage_position(&self) -> Result<Option<TradeResult>, KisError> {
    if self.is_stopped() {
        let result = self.close_position_market(ExitReason::EndOfDay).await?;
        return Ok(Some(result));
    }
    ...
}
```

수정:
```rust
async fn manage_position(&self) -> Result<Option<TradeResult>, KisError> {
    if self.is_stopped() {
        let is_manual = self.state.read().await.manual_intervention_required;
        if is_manual {
            warn!(
                "{}: manual_intervention 활성 — manage_position 자동 청산 스킵",
                self.stock_name
            );
            self.persist_active_position_for_recovery().await;
            if let Some(ref el) = self.event_logger {
                el.log_event(
                    self.stock_code.as_str(),
                    "position",
                    "auto_close_skipped",
                    "warn",
                    "manual_intervention 활성 — manage_position 진입 차단",
                    serde_json::json!({"path": "manage_position"}),
                );
            }
            return Ok(None);
        }
        let result = self.close_position_market(ExitReason::EndOfDay).await?;
        return Ok(Some(result));
    }
    ...
}
```

**왜 두 곳 모두인가**: reconcile에서 `stop_flag.store(true) + notify_waiters()` 이후 러너 메인 루프의 다음 iteration이 `manage_position`보다 **먼저** 1253 분기를 탄다. manage_position만 막으면 1253에서 그대로 청산된다.

**shared `position_lock` 유지 원칙**: `manual_intervention` 경로에서는 `close_position_market()`를 호출하지 않으므로 기존 held lock을 **해제하지 않는다**. unresolved live position이 남아 있는 동안 다른 종목 러너가 새 진입을 시도하면 더 위험해지므로, Day+0 핫픽스는 "자동 청산 스킵 + lock 유지 + 운영자 수동 처리"를 한 세트로 본다.

**`persist_active_position_for_recovery`**: 기존 `save_active_position`의 호출 helper. 새 메서드가 필요 없으면 직접 호출하고, 이미 최신 상태가 DB에 저장돼 있다면 불필요한 추가 write를 만들지 않도록 최소화한다. 목적은 `active_positions`를 유지해 운영자 개입 또는 재시작 시 `check_and_restore_position` 경로로 복구 가능하게 하는 것이다.

### P1 — confirm_reconcile_qty: 첫 일치 시 조기 종료

`strategy.rs:446-454` 시그니처 동일, 내부 로직 교체:
```rust
async fn confirm_reconcile_qty(
    &self,
    client: &KisHttpClient,
    code: &str,
    expected_qty: u64,   // 신규 파라미터: runner_qty 전달, 첫 일치 시 조기 종료 판정용
) -> Result<u64, String> {
    let attempts = self.reconcile_confirm_attempts.max(1);            // 기본 3
    let interval = std::time::Duration::from_secs(
        self.reconcile_confirm_interval_secs.max(2)                  // 기본 5
    );
    let mut last_qty = 0u64;
    for attempt in 0..attempts {
        if attempt > 0 {
            tokio::time::sleep(interval).await;
        }
        let holdings = self.fetch_balance(client).await
            .map_err(|e| format!("재확인 조회 실패(attempt {}): {e}", attempt + 1))?;
        last_qty = holdings.get(code).map(|(_, q)| *q).unwrap_or(0);

        // 첫 일치 시 조기 종료 — 회복이 빠르면 mismatch 판정 지연을 만들지 않는다
        if last_qty == expected_qty {
            return Ok(last_qty);
        }
    }
    Ok(last_qty)
}
```

호출부(`reconcile_once` 내부)에서 `runner_qty`를 인자로 전달하도록 수정.

`StrategyManager`에 두 필드 추가:
- `reconcile_confirm_attempts: u32` (`KIS_RECONCILE_CONFIRM_ATTEMPTS`, 기본 3)
- `reconcile_confirm_interval_secs: u64` (`KIS_RECONCILE_CONFIRM_INTERVAL`, 기본 5)

`config.rs`의 `AppConfig`에 위 두 필드 추가, 환경변수로 로드. `dummy_config()`도 갱신. paper/real 모두 동일 기본값으로 시작 (paper에서만 길게 가는 분기는 burn-in 결과 본 후 검토).

## 검증 체크리스트 (배포 전)

- [ ] `cargo fmt -- --check` 통과
- [ ] `cargo clippy` 신규 경고 0
- [ ] `cargo test --lib` 전체 통과 (현재 145+ pass)
- [ ] **신규 유닛테스트 (P0)**:
  - 메인 루프: `stop_flag=true + manual_intervention_required=true + current_position.is_some()` → `close_position_market` 호출 0회, `save_active_position` 1회, `auto_close_skipped` 이벤트 1건, 러너 break
  - 메인 루프: `stop_flag=true + manual_intervention_required=false + current_position.is_some()` → 기존 동작 유지(`close_position_market` + `save_trade_to_db`)
  - manage_position: 동일 매트릭스 (manual=true → `Ok(None)` + skip 이벤트, manual=false → 기존 EndOfDay 청산)
  - manual 경로에서는 shared `position_lock`이 해제되지 않고 유지되는지 확인
- [ ] **신규 유닛테스트 (P1)**:
  - 1차 조회에서 일치 → `Ok(expected)`, sleep 0회, `fetch_balance` 1회 호출
  - 1차 0, 2차 일치 → `Ok(expected)`, sleep 1회, `fetch_balance` 2회 호출
  - 3회 모두 불일치 → `Ok(last_qty)`, sleep 2회, `fetch_balance` 3회 호출
- [ ] `backtest 122630,114800 --days 365 --rr 2.5 --multi-stage --dual-locked` 결과를 핫픽스 전후 diff — **거래 수/entry/exit/pnl 완전 동일**해야 함 (라이브 전용 경로만 수정)
- [ ] `replay 122630 --date 2026-04-16` 결과 첫 거래 +0.48% 유지 확인
  - 단, 이 replay는 **보조 근거**일 뿐 hotfix 합격 기준의 본체는 아님. 실제 합격 기준은 P0/P1 유닛테스트와 backtest/replay 경로 무영향 확인에 둔다
- [ ] `cp target/release/kis_agent.exe target/release/kis_agent_rollback.exe` 백업

## 배포 (Day+0 단일)

1. P0 + P1 통합 한 PR/커밋. (P0를 두 경로 분리 커밋도 가능 — bisect 용이성 우선이면 분리 권장)
2. 기존 `target/release/kis_agent.exe` 롤백 백업
3. 다음 거래일(2026-04-20 월) 09:00 전 새 바이너리 기동
4. 장중 모니터링 — `auto_close_skipped` 이벤트 발생 시 즉시 운영자 알림(원래 자동 청산됐을 시나리오가 manual로 전환된 것). HTS에서 잔고 확인 필요
5. 장 마감 후 `docs/monitoring/2026-04-20.md`에 burn-in 2일차 결과 기록

## 롤백

증상 발생 시:
1. `/api/v1/strategy/stop` 또는 Ctrl+C
2. `target/release/kis_agent.exe ← kis_agent_rollback.exe` swap
3. 재시작 — `check_and_restore_position`이 `active_positions` 복구
4. DB 스키마 변경 없음, 추가 마이그레이션 불필요

## Follow-up (별도 PR로 분리)

리뷰에서 분리 결정한 항목들. 손실 재발 방지와 직접 연결되지 않거나, P0가 적용된 후에는 가치가 약해지는 것들.

### F1 — Day+1 (빠른 follow-up)

| 항목 | 사유 |
|---|---|
| **P3: `trades.environment` 하드코딩 제거** | 손실 방지 아님. 단 사고 포렌식 시 `environment=paper` 컬럼이 실제 환경과 무관하게 찍혀 분석 혼동 유발(이번 분석에서 실제 발생). `LiveRunnerConfig`에 `environment: Environment` 추가 + `Environment::Display` impl + `live_runner.rs:3129` 동적 참조. 변경 라인 적고 위험 낮음 → 다음 작업일 즉시 처리 권장 |

### F2 — Day+7 이후 (burn-in 결과 본 후 검토)

| 항목 | 사유 |
|---|---|
| **P2: `ExitReason::ReconcileMismatch` variant** | P0 적용 후에는 reconcile 강제 청산이 발생 안 함 → variant의 즉시 가치 0. 향후 reconcile 외 별도 강제 청산 경로가 생기면 도입 |
| **P4: cancel/fill race 후 60초 reconcile grace** | P0가 손실 경로를 끊으면 mismatch가 critical로 찍혀도 손실 무발생. grace의 가치는 "알람 노이즈 감소" 수준으로 떨어짐. 반면 진짜 hidden position 탐지를 60초 늦추는 trade-off가 발생 → burn-in 알람 빈도 데이터 본 후 결정 |
| **P5: cancel 전 `inquire-psbl-rvsecncl` 사전 조회** | check-then-act라 race 원천 제거 못 함. paper rate limit 자극. 이번 사고에서도 cancel_all_pending_orders fallback이 정상 작동(95주 체결을 정확히 인지)했으므로 cancel 경로 자체는 race 처리 중. 우선순위 낮음 |

## 수정 대상 파일 요약 (Day+0)

| 파일 | 변경 |
|---|---|
| `src/strategy/live_runner.rs` | P0 메인 루프(1252-1264) + P0 manage_position(1938-1943). `persist_active_position_for_recovery` helper 또는 기존 `save_active_position` 직접 호출 |
| `src/presentation/routes/strategy.rs` | P1 `confirm_reconcile_qty` 시그니처 변경(expected_qty 추가) + 내부 로직 교체, `reconcile_once`에서 `runner_qty` 전달, `StrategyManager` 두 필드 추가 |
| `src/config.rs` | P1 env 두 개(`KIS_RECONCILE_CONFIRM_INTERVAL`, `KIS_RECONCILE_CONFIRM_ATTEMPTS`), `AppConfig` + `dummy_config` 갱신 |

## 실행 순서 (승인 후)

1. P0 메인 루프 분기(`live_runner.rs:1252-1264`) 수정 + 유닛테스트
2. P0 manage_position 분기(`live_runner.rs:1938-1943`) 수정 + 유닛테스트
3. P1 `confirm_reconcile_qty` + `reconcile_once` 호출부 + `StrategyManager` 필드 + `config.rs` env 추가 + 유닛테스트
4. `cargo fmt && cargo clippy && cargo test --lib`
5. backtest 회귀 검증 (해시/거래수 일치) + replay 122630 4/16 +0.48% 유지 확인
6. release 빌드 + 롤백 백업
7. 커밋 (한국어 메시지, P0와 P1 분리 권장) + push 여부는 사용자 확인
