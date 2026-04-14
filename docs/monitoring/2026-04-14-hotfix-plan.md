# 2026-04-14 라이브 러너 거래 0건 사고 — 대응 계획 (v2, 리뷰 반영)

## Context

2026-04-14 장에서 122630(KODEX 레버리지)가 OR 상향 돌파 후 +2.9%까지 상승한 추세장이었으나 **거래 0건**. 동일 FVG(gap [94,300, 94,460])에 대해 `entry=94,465` 지정가가 **282회** 발주·미체결·취소 반복. 현재가 95,520원(괴리 1,055원) 상태에서도 같은 지정가를 발주한 구조 결함.

네 가지 독립 원인:
- **P0**: `signal_state.search_after`가 **청산 시에만** 전진(`live_runner.rs:788`). 미체결 경로 미갱신 → 같은 FVG 5초 polling마다 재탐지.
- **P1**: 지정가 발주 시 **현재가 미검증** → zone 이탈 후에도 같은 가격 발주.
- **P2**: WebSocket 분봉 수집 09:46 중단 후 **복구 경로 없음** → 후속 FVG 탐지 불가 (단, WS 자동 재연결은 100% 작동 확인).
- **P3**: WS 체결통보 AES key/iv 미설정 → REST 의존, REST도 장애 시 체결 오판.

프로세스 3번 재시작은 사용자 수동 조작으로 스코프 밖.

## v1 계획 대비 주요 변경점 (에이전트 리뷰 반영)

- **`abort_entry` 시그니처 분화**: `(signal_time, advance_search: bool, reason)` — 사유별로 `search_after` 전진 여부 분리 (preempt/API 에러는 전진 금지, drift/미체결만 전진).
- **P1 임계값 완화**: 0.003 → **0.005** (ETF 호가 단위와 5분봉 내부 변동 고려, 과도 차단 방지).
- **P1 config flag로 Day+0엔 OFF**, Day+2에 활성화 (리스크 분산).
- **P1 config를 `OrbFvgConfig`가 아닌 `LiveRunnerConfig` 또는 live_runner 모듈 const**로 분리 (백테스트 경로 영향 0 보장).
- **반복 발주 자동 안전장치 (신규 MUST)**: 최근 발주 큐 + "5분 내 동일가 3회+" → 해당 러너 즉시 stop.
- **Preempt 분기**: `KisError::Preempted` variant 신설. lock만 해제, `search_after` 전진 금지.
- **P2/P3 단기는 Day+0 배포에서 제외**. 모의투자 1 req/sec rate limit과 `inquire-balance` 불안정성 고려.
- **event_logger 이벤트 타입 확장**: `abort_entry`/`drift_rejected`/`repeated_order` 분류.
- **중장기 재설계 분리 티켓**: FVG 상태 머신 + tick-driven entry, 백테스트 보수화(zone×body 교집합), 지정가 정책 재검토(mid_price + IOC).

## Phase별 배포 계획

### Day+0 — 다음 거래일 전 핫픽스 (최소 Viable)

**변경 범위**: `src/strategy/live_runner.rs` 중심, `src/domain/errors.rs`(KisError variant), `src/infrastructure/monitoring/event_logger.rs`.

1. **P0 (search_after 전진, 재설계 반영)**
   - `best_signal` 튜플에 `signal_time: NaiveTime` 추가 — FVG 리트레이스 확인 캔들 `c5.time` (Agent 2 권고: 가독성 위해 `struct BestSignal` 도 고려).
   - 신규 헬퍼 `abort_entry(signal_time: NaiveTime, advance_search: bool, reason: &str)`:
     ```rust
     async fn abort_entry(&self, signal_time: NaiveTime, advance_search: bool, reason: &str) {
         if advance_search {
             let mut ss = self.signal_state.write().await;
             let new_after = ss.search_after.map_or(signal_time, |t| t.max(signal_time));
             ss.search_after = Some(new_after);
         }  // scope 분리 → deadlock 방지
         if let Some(ref lock) = self.position_lock {
             *lock.write().await = PositionLockState::Free;
         }
         info!("{}: 진입 중단 ({}) — advance_search={}", self.stock_name, reason, advance_search);
         if let Some(ref el) = self.event_logger {
             el.log_event(self.stock_code.as_str(), "strategy", "abort_entry", "info",
                 reason, serde_json::json!({ "signal_time": signal_time.to_string(), "advance_search": advance_search }));
         }
     }
     ```
   - `execute_entry` Err 경로(`live_runner.rs:1099~1114`)에서 사유별 분기:
     - `KisError::Internal("지정가 매수 미체결")` → `abort_entry(.., true, "fill_timeout")`
     - `KisError::Preempted` → `abort_entry(.., false, "preempted")` (신규, 아래 단계 3 참조)
     - API retryable Err 재시도 실패 → `abort_entry(.., false, "api_error")`
     - 진입 마감 임박(`execute_entry:1339~1342`) → `abort_entry(.., true, "cutoff")`
   - `signal_time` = `c5.time` (FVG 리트레이스 확인 5분봉). `c.time > search_after` 필터가 해당 캔들 정확 제외.

2. **반복 발주 자동 안전장치 (신규)**
   - `LiveRunner` 필드: `recent_orders: Arc<Mutex<VecDeque<(NaiveTime, i64)>>>` 크기 상한 10.
   - `execute_entry` 발주 성공 직후(`live_runner.rs:1452` 근방) push + 5분 이상 경과 pop.
   - 체크: 최근 5분 내 같은 `limit_price ±0.1%` 주문이 3회 이상 → `error!` 로그 + `event_logger "repeated_order" critical` + `self.stop_flag.store(true)` → 해당 종목 러너만 graceful stop. 전체 중단은 하지 않음(dual-locked의 나머지 종목은 살림).
   - 구현 ~20줄. 이 수정으로 P0/P1이 놓치는 다른 미지 버그에 대해서도 **최종 방어선** 확보.

3. **Preempt 에러 분기 (신규)**
   - `KisError::Preempted` enum variant 추가 (`src/domain/errors.rs` 또는 해당 파일).
   - `execute_entry` 체결 대기 루프의 preempt 감지(`live_runner.rs:1475~1481`)에서 break 후 반환할 에러를 `KisError::Preempted`로 변경 (현재는 `Internal("지정가 매수 미체결")`와 구분 불가).
   - Agent 2 발견: preempt는 "다른 종목에 기회 양보"이지 FVG 자체 무효가 아니므로 `search_after` 전진 금지.

4. **P1 framework만 추가, 기본 비활성**
   - `OrbFvgConfig`에는 **손대지 않음**. `live_runner.rs` 모듈 수준 상수 또는 `LiveRunnerConfig { max_entry_drift_pct: Option<f64> }` (default `None`).
   - `poll_and_enter`의 `execute_entry` 호출 직전(`live_runner.rs:1078~1088`)에 가드 placeholder:
     ```rust
     if let Some(threshold) = cfg.max_entry_drift_pct {
         match self.get_current_price().await {
             Ok(cur) => {
                 let drift = match side {
                     PositionSide::Long  => (cur - entry_price) as f64 / entry_price as f64,
                     PositionSide::Short => (entry_price - cur) as f64 / entry_price as f64,
                 };
                 if drift > threshold {
                     warn!("{}: FVG zone 이탈 — current={}, entry={}, drift={:.2}%",
                         self.stock_name, cur, entry_price, drift * 100.0);
                     self.abort_entry(signal_time, true, "drift_exceeded").await;
                     if let Some(ref el) = self.event_logger {
                         el.log_event(.., "drift_rejected", .., json!({"drift": drift, "current": cur, "entry": entry_price}));
                     }
                     return Ok(false);
                 }
             }
             Err(e) => {
                 // fail-close: 현재가 모르는 상태에서 지정가 발주 위험
                 warn!("{}: 현재가 조회 실패 — 진입 포기: {e}", self.stock_name);
                 self.abort_entry(signal_time, false /* 전진 X: 일시 장애 */, "price_fetch_failed").await;
                 return Ok(false);
             }
         }
     }
     ```
   - Day+0 배포 시 `max_entry_drift_pct = None` → placeholder만, 실제 차단 없음.

5. **event_logger 이벤트 타입 확장**
   - `src/infrastructure/monitoring/event_logger.rs`에 event_type enum/상수 목록에 `abort_entry`, `drift_rejected`, `repeated_order` 추가. DB 스키마 변경 없음 (`event_log` 테이블의 `event_type` 컬럼은 VARCHAR).

**배포 전 검증**:
- `backtest 122630,114800 --days 365 --rr 2.5 --multi-stage --dual-locked` 실행 후 결과 해시 기록.
- 핫픽스 빌드로 동일 명령 재실행 → 해시 비교 일치 필수. 불일치 시 배포 중단 (라이브 전용 경로 침범 증거).
- 2026-04-14 당일 재현 `backtest 122630 --days 1 --dual-locked` → 거래 2건/본전 결과 유지.
- 기존 `target/release/kis_agent.exe` → `target/release/kis_agent_rollback.exe`로 복사(rollback 안전판).

### Day+1 관찰 → Day+2 — P1 활성화

- 하루 관찰 후 `LiveRunnerConfig.max_entry_drift_pct = Some(0.005)`로 활성화.
- 지표:
  - `abort_entry` 일일 호출 수: 정상 5~20건 / 급등장 50건 이하 / >100건이면 임계값 재튜닝.
  - `drift_rejected` 이벤트: 추세장에서 1~3건 예상.

### Day+7 — P2 재설계 배포 (TPS 고려)

- 원래 계획의 "30초 쓰로틀"은 모의투자 1 req/sec 한계에 위험하므로 재설계:
  - `fetch_candles_split`(line 2247)에서 "WS 무음 30s + 최근 5분봉 미존재" AND 조건만 trigger.
  - **쓰로틀 60~90초**, 최대 페이지 **1**(현재 3에서 축소).
  - backfill 결과를 `ws_candles` HashMap에 주입(aggregator `inject_completed_candles` API 추가 필요) — 안 하면 매 사이클 cache-miss.
  - Agent 3 분석: WS 자동 재연결 100% 작동 이력 → 당장 필요성 낮음, 재설계 후 배포로 충분.

### Day+30+ — 후속 과제 (별도 티켓)

- **P3 단기** (잔고 교차검증): 모의투자 `inquire-balance` 안정성 관찰 후. 우선 `query_execution` **재시도 2~3회 + 500ms backoff** 강화가 선행.
- **P3 장기** (AES 핸드셰이크): approval_key 응답의 `aes_key/aes_iv` 파싱·저장 + 체결통보 AES-256-CBC 복호. 별도 WebSocket 리팩터.
- **지정가 정책 재검토**: `gap.top` 고정 → `mid_price` 기본 + zone 이탈 시 `current + k·tick` IOC 하이브리드 (Agent 1 3번 논점).
- **백테스트 보수화**: `orb_fvg.rs::scan_and_trade`에서 `gap.contains_price(c5.low)` → `zone × body 교집합` 요구로 변경. 라이브 현실 반영 (Agent 1 2번 논점).
- **FVG 상태 머신 + tick-driven entry (v2 엔진)**: `active_fvgs: Vec<FvgEntry>` + WebSocket tick 기반 진입 트리거 분리. `poll_and_enter`는 FVG 등록만 담당 (Agent 1 1·4번 논점).
- **Regression test harness**: 2026-04-14 데이터로 "94,465 발주 ≤ 1회" assert. 향후 사고마다 케이스 누적.

## 수정 파일 요약

| 파일 | Day+0 변경 내용 |
|---|---|
| `src/strategy/live_runner.rs` | `best_signal` 튜플 확장, `abort_entry` 헬퍼, Err 분기 리팩터, `recent_orders` 필드, P1 placeholder, `LiveRunnerConfig` 신설 |
| `src/domain/errors.rs` (또는 `KisError` 정의 파일) | `Preempted` variant 추가 |
| `src/infrastructure/monitoring/event_logger.rs` | `abort_entry`/`drift_rejected`/`repeated_order` event_type 추가 |
| `src/strategy/orb_fvg.rs` | **변경 없음** (백테스트 영향 방지) |

## 관찰 지표 & 알람

| 지표 | 정상 | 경고 | 비상 |
|---|---|---|---|
| `abort_entry` 일일 | 5~20 | 20~100 | >100 (P1 임계값 재튜닝) |
| 동일가 5분 내 발주 | 1~2 | 3 | 4+ (자동 runner stop) |
| `drift_rejected` 일일 | 0~3 | 3~10 | >10 (지정가 정책 재검토 트리거) |
| WS 분봉 stale >3min | 0 | 1~2 | 3+ (P2 우선순위 상향) |
| `repeated_order` critical | 0 | 1 | 2+ (즉시 조사) |

- 모든 이벤트는 `event_logger`에 DB 저장 → `/monitoring` 라우트에서 daily count 노출 (별도 PR로 UI 추가 권장).

## Rollback 절차

1. 증상 확인 → 해당 종목 러너 stop (`/api/strategy/stop`) 또는 전체 Ctrl+C.
2. `target/release/kis_agent.exe` ← `kis_agent_rollback.exe` swap.
3. 재시작 → `check_and_restore_position`이 orphan 자동 정리(CLAUDE.md 2026-04-14 추가분).
4. DB 스키마 변경 없으므로 추가 조치 불필요.

## 검증 체크리스트 (Day+0 배포 전)

- [ ] `cargo test`(기존 유닛 테스트) 통과
- [ ] `abort_entry` 유닛 테스트: `advance_search=true/false` 각각 `search_after` 상태 검증
- [ ] `recent_orders` 유닛 테스트: 동일가 3회 트리거 시 stop_flag 세트
- [ ] `cargo clippy`/`cargo fmt --check` 통과
- [ ] `backtest --days 365` 결과 해시 일치
- [ ] `backtest 122630 --days 1 --dual-locked`(2026-04-14 재현) 2거래/본전 유지
- [ ] rollback 바이너리 백업 완료

## 수정하지 않는 부분 (의도)

- **지정가 매수 정책 자체** (CLAUDE.md 2026-04-14 변경) — Day+0에서는 동일 유지. 중장기 재검토 항목으로 분리.
- **`fvg_expiry_candles=6`**, **RR 2.5**, **트레일링 0.05R** 등 전략 파라미터 — 오늘 사고는 진입 실패이지 청산 로직 문제 아님.
- **프로세스 재시작 복구 로직** (`live_runner.rs:640~650`) — 사용자 수동 재시작, 스코프 외.
- **백테스트 `scan_and_trade` 로직** — 라이브 경로만 수정, 해시 비교로 증명.

---

## 구현 작업 체크리스트 (Day+0 핫픽스)

의존성 순서대로 정렬. 각 항목은 독립 커밋으로 분리해 bisect 용이성 확보.

### 1. `KisError::Preempted` variant 추가

- [ ] **파일 위치 확인**: `KisError` enum 정의 파일 찾기 (추정: `src/domain/errors.rs` 또는 `src/infrastructure/kis_client/errors.rs`)
- [ ] `Preempted` variant 추가 (데이터 없음 또는 `{ by_stock: String }`)
- [ ] `Display`/`Debug` impl에서 문구 추가 — "다른 종목 선점 요청으로 진입 포기"
- [ ] `is_retryable()` 메서드에 `Preempted => false` 명시

### 2. `LiveRunnerConfig` 신설 + `LiveRunner` 필드 확장

- [ ] `src/strategy/live_runner.rs`에 새 struct:
  ```rust
  #[derive(Debug, Clone, Default)]
  pub struct LiveRunnerConfig {
      /// zone 이탈 허용 한계. None이면 가드 비활성 (Day+0 기본)
      pub max_entry_drift_pct: Option<f64>,
  }
  ```
- [ ] `LiveRunner` 구조체에 필드 추가:
  - `live_cfg: LiveRunnerConfig` (default `None`)
  - `recent_orders: Arc<Mutex<VecDeque<(NaiveTime, i64)>>>` (상한 10)
- [ ] `LiveRunner::new` 생성자 업데이트: 두 필드 초기화
- [ ] `with_live_config(cfg: LiveRunnerConfig) -> Self` 빌더 메서드 추가 (Day+2에서 활성화용)

### 3. `abort_entry` 헬퍼 구현

- [ ] `LiveRunner` impl에 메서드 추가:
  ```rust
  async fn abort_entry(&self, signal_time: NaiveTime, advance_search: bool, reason: &str) { ... }
  ```
- [ ] 동작:
  - `advance_search=true` → `signal_state.search_after = Some(max(기존, signal_time))` (write scope 분리)
  - `position_lock` write → `Free` (write scope 분리 — deadlock 방지)
  - `info!` 로그 + `event_logger.log_event("strategy", "abort_entry", "info", reason, {signal_time, advance_search})`

### 4. `best_signal` 튜플 → `struct BestSignal` 변환

- [ ] `live_runner.rs:945` 선언부:
  ```rust
  struct BestSignal {
      side: PositionSide,
      entry_price: i64,
      stop_loss: i64,
      take_profit: i64,
      stage_name: &'static str,
      signal_time: NaiveTime,
  }
  let mut best_signal: Option<BestSignal> = None;
  ```
- [ ] `live_runner.rs:1025~1027`에서 struct literal로 생성 (`signal_time: c5.time`)
- [ ] `live_runner.rs:1035` destructure 대신 field 접근
- [ ] `live_runner.rs:1080~1085` 로그 포맷 자동 호환

### 5. `poll_and_enter` Err 분기를 `abort_entry`로 수렴

- [ ] `live_runner.rs:1089~1115` `execute_entry` match arm 재작성:
  - `Ok(_)` → `return Ok(true)` (유지)
  - `Err(KisError::Preempted)` → `abort_entry(signal_time, false, "preempted")` → `return Ok(false)`
  - `Err(e) if e.is_retryable()` → 기존 1회 재시도 → 재시도도 Err → `abort_entry(signal_time, false, "api_error")` (search_after 전진 금지)
  - `Err(_)` (미체결/cutoff 등) → `abort_entry(signal_time, true, "fill_timeout")` → `return Ok(false)`

### 6. Preempt 에러 반환 경로 수정

- [ ] `live_runner.rs:1475~1481` 체결 대기 루프의 preempt 감지 후 break → 루프 탈출 뒤 별도 분기:
  ```rust
  let preempted = { /* lock read로 preempted 플래그 확인 */ };
  if preempted && filled_qty == 0 {
      // 취소 로직 실행 후
      return Err(KisError::Preempted);
  }
  ```
- [ ] `live_runner.rs:1546` 기존 `Err(KisError::Internal("지정가 매수 미체결"))` 경로는 timeout/cutoff 케이스 전용으로 유지.

### 7. P1 drift 가드 placeholder 삽입

- [ ] `live_runner.rs:1078~1088` `execute_entry` 호출 직전에 조건부 가드:
  ```rust
  if let Some(threshold) = self.live_cfg.max_entry_drift_pct {
      match self.get_current_price().await {
          Ok(cur) => {
              let drift = /* side별 계산 */;
              if drift > threshold {
                  self.abort_entry(best_signal.signal_time, true, "drift_exceeded").await;
                  // event_logger.log_event "drift_rejected"
                  return Ok(false);
              }
          }
          Err(_) => {
              // fail-close, lock만 해제 (일시 장애)
              self.abort_entry(best_signal.signal_time, false, "price_fetch_failed").await;
              return Ok(false);
          }
      }
  }
  ```
- [ ] Day+0에선 `max_entry_drift_pct = None`이라 스킵. 코드 경로만 배포.

### 8. 반복 발주 자동 안전장치 구현

- [ ] `live_runner.rs:1452` 근방 (지정가 발주 성공 직후) 훅:
  ```rust
  {
      let mut queue = self.recent_orders.lock().await;
      let now = Local::now().time();
      queue.retain(|(t, _)| (now - *t).num_seconds() < 300); // 5분 창
      queue.push_back((now, limit_price));
      while queue.len() > 10 { queue.pop_front(); }

      let dup_count = queue.iter()
          .filter(|(_, p)| ((p - limit_price).abs() as f64) / limit_price as f64 <= 0.001)
          .count();
      if dup_count >= 3 {
          error!("{}: 동일가 {}원 5분 내 {}회 발주 감지 — runner 자동 중단",
              self.stock_name, limit_price, dup_count);
          if let Some(ref el) = self.event_logger {
              el.log_event(self.stock_code.as_str(), "strategy", "repeated_order", "critical",
                  &format!("동일가 {}원 {}회 반복", limit_price, dup_count),
                  serde_json::json!({"price": limit_price, "count": dup_count}));
          }
          self.stop_flag.store(true, Ordering::SeqCst);
          self.stop_notify.notify_waiters();
      }
  }
  ```

### 9. event_logger 이벤트 타입 상수 추가

- [ ] `src/infrastructure/monitoring/event_logger.rs`에서 event_type 문자열 상수 또는 enum에 추가:
  - `"abort_entry"`, `"drift_rejected"`, `"repeated_order"`
- [ ] DB 스키마 변경 없음 확인 (`event_log.event_type`이 VARCHAR).

### 10. 유닛 테스트

- [ ] `tests/live_runner_abort_entry.rs` (또는 `live_runner.rs` 내부 `#[cfg(test)] mod tests`):
  - `advance_search=true` + `search_after=None` → `search_after == Some(signal_time)`
  - `advance_search=true` + `search_after=Some(이전)` → `max(이전, signal_time)`
  - `advance_search=false` → `search_after` 불변
  - 두 케이스 모두 `position_lock == Free`
- [ ] `tests/repeated_order_guard.rs`:
  - 동일가 2회 발주 후 `stop_flag == false`
  - 동일가 3회 → `stop_flag == true` + event_logger mock에 critical 이벤트 1건
  - 5분 초과 후 오래된 항목 pop 확인

### 11. 빌드·검증

- [ ] `cargo fmt -- --check` 통과
- [ ] `cargo clippy` 경고 0 (기존 warning은 허용)
- [ ] `cargo test --lib` 전체 통과
- [ ] `cargo build --release` 빌드 성공

### 12. 백테스트 회귀 검증

- [ ] 핫픽스 전 바이너리로:
  ```
  ./target/release/kis_agent.exe backtest 122630,114800 --days 365 --rr 2.5 --multi-stage --dual-locked > before.txt
  ```
- [ ] 핫픽스 빌드 후 동일 명령 → `after.txt`
- [ ] `diff before.txt after.txt` — **거래 수·entry·exit·pnl 완전 동일**이어야 함.
- [ ] 2026-04-14 재현:
  ```
  ./target/release/kis_agent.exe backtest 122630,114800 --days 1 --rr 2.5 --multi-stage --dual-locked
  ```
  → 122630 2거래(승1 패1, 본전), 114800 0거래 유지 확인.

### 13. 배포 준비

- [ ] `cp target/release/kis_agent.exe target/release/kis_agent_rollback.exe`
- [ ] 커밋 로그에 hotfix 사유 + PR #번호 명시
- [ ] `docs/monitoring/2026-04-14.md`에 hotfix 배포 기록 추가

---

## Day+2 작업 (관찰 후 P1 활성화)

- [ ] `main.rs` 또는 `LiveRunner::new` 호출부에서:
  ```rust
  let live_cfg = LiveRunnerConfig {
      max_entry_drift_pct: Some(0.005),
  };
  // runner.with_live_config(live_cfg)
  ```
- [ ] rebuild + 재시작
- [ ] 장중 `drift_rejected` 이벤트 카운트 모니터링
- [ ] 일 마감 후 결과 리뷰 → 임계값 유지 또는 조정

## Day+7 작업 (P2 재설계)

- [ ] `fetch_candles_split` stale 감지 로직 (WS 30s 무음 AND 최근 5분봉 미존재)
- [ ] `fetch_minute_candles_from` 호출 시 `max_pages=1`로 제한
- [ ] 쓰로틀 60~90s `Mutex<Option<Instant>>` 추가
- [ ] `CandleAggregator::inject_completed_candles(bars)` API 추가
- [ ] backfill 결과를 aggregator에 주입 (cache-miss loop 방지)
- [ ] rate limiter 부하 관찰 (모의투자 1 req/sec 기준)

## Day+30+ 후속 티켓 (각각 별도 PR)

- [ ] **Ticket A**: `query_execution` 재시도 2~3회 + 500ms backoff 강화
- [ ] **Ticket B**: P3 단기 잔고 교차검증 (A 완료 후, `inquire-balance` 안정성 데이터 수집 후)
- [ ] **Ticket C**: P3 장기 AES 핸드셰이크 (approval_key 응답 파싱·저장·복호)
- [ ] **Ticket D**: 백테스트 `scan_and_trade` 보수화 (zone×body 교집합)
- [ ] **Ticket E**: 지정가 정책 재검토 (mid_price + IOC slippage 하이브리드)
- [ ] **Ticket F**: FVG 상태 머신 + tick-driven entry v2 엔진
- [ ] **Ticket G**: Regression test harness (2026-04-14 replay assert)
- [ ] **Ticket H**: `/monitoring` 라우트에 daily event 카운트 UI
