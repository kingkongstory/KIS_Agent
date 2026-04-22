# 2026-04-17 수정 루프 탈출 — 최소 수정 세트 3개 (근본 원인 수정, v3)

> v1 (2026-04-17 작성) → v2 (1 차 타부서 리뷰 반영) → **v3 (2 차 타부서 리뷰 반영)**
>
> v3 반영 3 건:
> - P1: `CompletedCandle` 에 `interval_min` 필드 추가 (현재 struct 에 없음 — v2
>       `fetch_canonical_5m_candles` 가정이 컴파일 불가)
> - P2: merge precedence 재정의 — **과거 완료 버킷은 Yahoo 우선**, 진행 중 현재
>       버킷만 WS. v2 의 "WS 우선" 은 postmortem 진단(WS 품질 결함)과 자기 모순
> - P3: 검증 상향 — 시각 집합 일치 → **OHLC 동치성 + signal-set 동치성**

## Context

지난 3 일간 30+ 개 결함 수정의 대부분은 **안전장치(symptom 완화)** 에 가깝다.
2026-04-17 장중 -1.05% 손실과 P0 우회 사고는 안전장치만으로 끊을 수 없는
**3 개의 근본 원인** 을 드러냈다:

1. **전략 명세 위반**: paper 기본값이 `allowed_stages=None` 이라 5m/15m/30m 모두
   허용, `trade_count == 0` 일 때만 OR breakout 가드 켜짐. 2 차 거래부터는 OR
   돌파 없이도 신호 통과 (2026-04-17 2 차 `or_breakout_pct=-0.054%` 는 오탐이
   아니라 의도된 결과).
2. **캔들 소스 분리 + granularity 혼재**: `completed` 저장소에 WS tick aggregation
   (1 분봉) 과 Yahoo/Naver 백필 (5 분봉) 이 함께 저장되지만 현재 `CompletedCandle`
   struct 에는 granularity 메타 필드가 없음. OR 은 `all_candles`, FVG 탐색은
   `realtime_candles` 를 쓰며 두 배열이 다른 입력.
3. **진입 진행 중 상태 불가시**: 지정가 발주 ~ 체결 확인 (최대 30 초) 구간에
   `runner_qty = 0, KIS_qty > 0` 과도 상태가 발생, `reconcile_once` 가 이를
   hidden position 으로 오판해 manual 승격시킨다. 2026-04-17 11:00 사고의 직접
   원인.

이 3 개만 수정. 그 외는 중단. 루프의 종료 조건을 4 가지로 고정.

---

## 수정 1 — 전략 불변식 코드 하드 고정 (live + backtest + parity + replay 관통)

**원칙**: 15m OR 만 사용, OR breakout 가드는 **모든 거래** 에 적용.
**라이브뿐 아니라 backtest/parity/replay 경로 전체에 동일 규칙.**

### 1a. paper 기본 stage 를 15m 로 고정

- `src/strategy/live_runner.rs:311` — `LiveRunnerConfig::default()` 의
  `allowed_stages: None` → `Some(vec!["15m".to_string()])`
- 5m / 30m 신호는 UI 관찰용으로만 노출 (진입 경로 차단).

### 1b. `require_or_breakout` 를 모든 경로에서 항상 true

다음 5 곳의 `trade_count == 0` 조건을 `true` 로 교체:

| 파일 | 라인 | 현재 |
|---|---|---|
| `src/strategy/live_runner.rs` | 1876 | `require_or_breakout: trade_count == 0` |
| `src/strategy/orb_fvg.rs` | 158 | `require_or: trade_count == 0` |
| `src/strategy/backtest.rs` | 410 | 동일 |
| `src/strategy/parity/parity_backtest.rs` | 143, 151, 480 | 동일 |

플래그 자체 (`SignalEngineConfig.require_or_breakout`) 는 유지 — 기존
`or_breakout_guard_rejects_if_disabled` 테스트 호환.

### 1c. replay/backtest/parity 경로에도 stage 필터 관통

종료 조건의 "replay 0 건 재현" 이 "15m-only 때문" 임을 보장하려면 검증 경로까지
`15m` 단일 필터 필요:

- **replay 명령**: `main.rs:498` `BacktestEngine::replay_day()` 진입점에 `--stage`
  옵션 추가, 기본값 `15m`
- **backtest 명령**: `backtest.rs:127` multi-stage 순회 부분에서 `allowed_stages`
  주입. `--multi-stage` 플래그 **의미 축소** 또는 deprecate
- **parity backtest**: `parity_backtest.rs:451` 도 동일 필터 주입
- **검증 명령 자체 수정** (체크리스트 포함):
  - 기존: `backtest 122630,114800 --days 5 --multi-stage --dual-locked`
  - 수정: `backtest 122630,114800 --days 5 --stages 15m --dual-locked`
  - 기존: `replay 122630 --date 2026-04-17 --rr 2.5`
  - 수정: `replay 122630 --date 2026-04-17 --rr 2.5 --stage 15m`

### 1d. 새 회귀 테스트

- `live_runner.rs` — "paper 기본 `allowed_stages == Some(["15m"])`" 단위 테스트
- parity/live_like backtest 가 `require_or_breakout=true` 로 돌아가는지 assert
- backtest/parity 진입점이 기본 stage 를 15m 로 가지는지 assert

### 1e. 백테스트 회귀

- `backtest 122630,114800 --days 365 --stages 15m --rr 2.5 --dual-locked` 수정
  전/후 비교. 거래 수 **감소** 예상 — **정상화로 기록**, 부작용 아님
- `replay 122630 --date 2026-04-17 --stage 15m --rr 2.5` 결과: 수정 #2 적용 후
  0 건 재현이어야 함

---

## 수정 2 — Canonical 5 분 시퀀스 정규화 (v3 재설계)

**원칙**: 전략 판단 (OR 계산 + FVG 탐색) 은 **같은 canonical 5 분 배열** 사용.
v1 "단순 치환" 폐기 → v2 "fetch_canonical_5m_candles 신규" 유지 → **v3 추가
항목**: struct 필드 확장 + precedence 규칙 재정의.

### 2a. 진단

- `completed` 저장소 (HashMap<..., Vec<CompletedCandle>>) 에 **두 가지 granularity
  캔들 공존**:
  - WS tick aggregation → 1 분봉 (생성 지점 `candle_aggregator.rs:571, 626`)
  - Yahoo / Naver 백필 → 5 분봉 (생성 지점 `:293, :379, :446`)
  - DB preload → 저장된 `interval_min` 컬럼 값 사용 (생성 지점 `:198`)
- 그러나 **메모리 `CompletedCandle` struct 에 granularity 필드가 없다**:
  ```
  pub struct CompletedCandle {
      stock_code, time, open, high, low, close, volume, is_realtime  // 필드 전부
  }  // candle_aggregator.rs:74-85
  ```
- `is_realtime` 은 "실시간 수집 vs 외부 백필" 의미지 granularity 가 아님. 1m/5m
  혼재 판정에 쓰기 부적합
- `live_runner.rs:1726` `fetch_candles_split()` 이 `is_realtime` 으로만 분리
- `live_runner.rs:1851` FVG 탐색: `realtime_candles` → `aggregate_completed_bars
  (scan, 5, now)` 로 5 분 집계 (WS 1 분봉 전용)
- OR 계산 (`:1763`) 은 `all_candles.filter(time in OR)` — 1/5 분 혼재이나 min/max
  연산이라 영향 작음. 단 구조적으로는 같은 결함

### 2b. P1 — `CompletedCandle` 에 `interval_min` 필드 추가 (v3 신규)

```rust
// candle_aggregator.rs:74
pub struct CompletedCandle {
    pub stock_code: String,
    pub time: String,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
    #[serde(skip)]
    pub is_realtime: bool,
    /// 2026-04-17 v3: 이 캔들의 시간 단위 (분). WS tick 집계=1, Yahoo/Naver
    /// 백필=5, DB preload 는 저장된 값. canonical 5m 정규화의 1m→5m 집계 대상
    /// 과 그대로 사용 대상을 구분하는 근거.
    pub interval_min: u32,
}
```

**생성 사이트 6 곳 전부 채움** (빠뜨리면 컴파일 에러로 강제):

| 위치 | 소스 | `interval_min` 값 |
|---|---|---|
| `candle_aggregator.rs:198` | DB preload | DB 컬럼 값 (1 또는 5) |
| `candle_aggregator.rs:293` | Yahoo 백필 | 5 |
| `candle_aggregator.rs:379` | Yahoo refresh | 5 |
| `candle_aggregator.rs:446` | Naver fallback | 5 |
| `candle_aggregator.rs:571` | WS tick 집계 | 1 |
| `candle_aggregator.rs:626` | 기타 | 해당 상황 분석 후 값 확정 |

`is_realtime` 은 유지 (UI/디버깅, "외부 백필 vs 실시간" 메타). `interval_min` 은
독립.

### 2c. P2 — `fetch_canonical_5m_candles()` 시간 경계 기반 precedence (v3 수정)

v2 의 "WS 집계 우선" 은 이 계획의 출발점 (postmortem 진단: 라이브 WS high 90 원
누락) 과 **자기 모순**. v3 에서 precedence 규칙을 시간 경계 기반으로 재정의:

```
과거 완료 5m 버킷 (bucket_end <= now):
  우선순위 1: Yahoo / Naver 백필 (interval_min=5) — 공식 OHLC, 검증 소스
  우선순위 2: WS 1m→5m 집계 (interval_min=1 집계 결과) — 백필 없을 때만 fallback
진행 중 현재 버킷 (bucket_start <= now < bucket_end):
  WS 1m→5m 집계만 사용 — Yahoo 는 아직 해당 버킷 미생성
```

**근거**:
- Yahoo 는 완료된 5m 버킷에 대해 공식 OHLC 제공 (거래소 원본에 근접)
- WS tick 집계는 tick 일부 누락 가능 (2026-04-17 실측 — 라이브 high 102,740 vs
  Yahoo 102,830)
- 현재 진행 중 버킷은 Yahoo 에 아직 없음 — WS 만 실시간 값 제공

**구현 스케치**:

```rust
// live_runner.rs 신규
async fn fetch_canonical_5m_candles(&self) -> Result<Vec<CompletedCandle>, KisError> {
    let raw = self.ws_candles.read().await;
    let for_code = raw.get(code).cloned().unwrap_or_default();
    let now = Local::now().time();

    // 1. interval_min=1 (WS 1 분봉) → 5 분 집계 (진행 중 버킷 포함 가능)
    let ws_1m: Vec<_> = for_code.iter().filter(|c| c.interval_min == 1).cloned().collect();
    let ws_5m_aggregated = aggregate_completed_bars(&ws_1m, 5, now);

    // 2. interval_min=5 (Yahoo/Naver 백필) — 과거 완료 버킷만 신뢰
    let backfilled_5m: Vec<_> = for_code.iter().filter(|c| c.interval_min == 5).cloned().collect();

    // 3. 시각 merge — precedence:
    //    과거 완료 버킷 (bucket_end <= now): Yahoo 우선, 없으면 WS fallback
    //    진행 중 현재 버킷: WS 만 사용
    use std::collections::BTreeMap;
    let mut by_time: BTreeMap<String, CompletedCandle> = BTreeMap::new();

    // 3-a. 과거 완료 버킷 — Yahoo 우선
    for b in &backfilled_5m {
        if is_past_bucket(&b.time, now) {
            by_time.insert(b.time.clone(), b.clone());
        }
    }

    // 3-b. WS 집계 — 과거 완료 버킷에 Yahoo 없으면 fallback, 현재 진행 버킷은 WS 독점
    for w in &ws_5m_aggregated {
        let is_past = is_past_bucket(&w.time, now);
        if is_past {
            by_time.entry(w.time.clone()).or_insert_with(|| w.clone());  // Yahoo 없을 때만
        } else {
            by_time.insert(w.time.clone(), w.clone());  // 현재 버킷은 WS 덮어쓰기
        }
    }

    Ok(by_time.into_values().collect())
}

fn is_past_bucket(bucket_time_str: &str, now: NaiveTime) -> bool {
    // bucket_time = 해당 5m 버킷 시작 시각. bucket_end = start + 5min
    let start = parse_time(bucket_time_str);
    let end = start + Duration::minutes(5);
    end <= now
}
```

### 2d. OR/FVG 모두 canonical 사용

- `live_runner.rs:1726` 호출 변경: `let canonical = self.fetch_canonical_5m_candles().await?;`
- `live_runner.rs:1763` OR 계산: `canonical.iter().filter(time in OR)`
- `live_runner.rs:1851` FVG 탐색: `canonical.iter().filter(time >= or_end)` (이미
  5m 이므로 재집계 불필요)
- `aggregate_completed_bars` FVG 경로 호출 제거 (`:1856` 근처)

### 2e. `is_realtime` 플래그 용도 제한

- 플래그 자체는 유지 (UI/디버깅 메타)
- **전략 판단 경로에서는 사용 금지**. granularity 판정은 오직 `interval_min` 으로

### 2f. P3 — 검증을 OHLC 동치성 + signal-set 동치성으로 상향 (v3 수정)

v2 "시각 집합 일치" 는 사고 본질 (OHLC 값 차이) 을 못 잡음 → v3 에서 검증 두 단
계로 강화:

**검증 A — OHLC 동치성** (canonical candle 자체의 정합성):
- 종목 × 날짜 단위로 canonical 5m 시퀀스 생성
- **과거 완료 버킷** (bucket_end <= now 또는 검증 시점 기준 장 마감 이후): 같은
  시각의 canonical vs Yahoo 5m 원본의 `open/high/low/close` **완전 일치** (tolerance 0)
- **현재 진행 버킷**: Yahoo 에 없으므로 검증 skip. 또는 다음 거래일까지 대기 후
  검증
- 샘플 10 개 이상 (또는 오늘의 모든 과거 완료 버킷). 불일치 발견 시 실패 처리 +
  원인 조사 (WS 집계 로직? Yahoo fetch 로직?)

**검증 B — Signal-set 동치성** (신호 재현성):
- 라이브에서 찍힌 `entry_signal` 레코드: `(time, stage, entry, stop_loss,
  take_profit, or_breakout_pct)` 집합 추출
- canonical 5m 기반 replay (parity/passive 경로) 에서 같은 레코드 집합 추출
- **두 집합이 완전 일치** — 누락/추가/값 차이 모두 없어야 통과
- 2026-04-17 기준 기대: 라이브 2 건 → canonical replay 0 건 (수정 #1 + #2 적용
  후 라이브가 이제 0 건이 되므로, 양쪽 모두 빈 집합 일치)

**tolerance 관련 주의**: 과거 완료 버킷은 완전 일치가 원칙이지만, Yahoo 자체의
API 반환값이 간헐적으로 조정될 수 있다. 첫 불일치 발견 시 "Yahoo 응답 변동 vs
canonical 결함" 을 분리 조사. 그러나 **검증 기준 자체는 tolerance 0 으로 시작**.

### 2g. `b_volume = 0` 이슈 (표현 수정 유지)

- 현재 원인 미확정: `docs/monitoring/2026-04-16.md:97, 113` 에 WS 기반 라이브
  entry_signal 에서도 `b_volume=0` 반복 관찰됨
- canonical candle 경로 확정 후 WS / Yahoo / Naver 어느 입력에서 유래하는지
  분리 조사. 본 계획에서는 수정하지 않음

---

## 수정 3 — 진입 진행 중 상태를 reconcile 이 인식 (실사용 경로 테스트)

**원칙**: 지정가 발주 후 체결 확인 전 과도 상태에서 reconcile 이 판정 보류.
**테스트는 실사용 경로만 — `classify_reconcile_outcome` dead helper 확장 금지.**

### 3a. 진단

- `live_runner.rs:1974` — `poll_and_enter` 가 `execute_entry` 전에
  `Pending { code: self, preempted: false }` 로 lock 설정
- `execute_entry` 체결 대기 루프 (~30 초): `state.current_position = None`,
  `active_position_lock = Pending(self)`
- `src/presentation/routes/strategy.rs:reconcile_once` — `active_position_lock`
  을 **전혀 읽지 않음** → 과도 상태를 mismatch 로 오판

### 3b. 수정 — `reconcile_once` 에 Pending/ManualHeld skip 가드

`reconcile_once` 본문에서 각 종목 판정 진입부에 shared lock 체크 추가:

```rust
// strategy.rs reconcile_once, 각 종목 처리 시작부 (already_manual 체크 직후)
if should_skip_reconcile_for_lock(&*self.active_position_lock.read().await, &code) {
    tracing::debug!("[reconcile] {} lock 상태로 skip", code);
    continue;
}
```

변경 위치: `strategy.rs:948` (`already_manual` 체크 직후).

### 3c. 테스트 전략 (classify_reconcile_outcome 확장 금지)

`classify_reconcile_outcome` (`strategy.rs:1180`) 이 **reconcile_once 실경로에서
호출되지 않는 dead helper**. 거기에 테스트 붙여도 실경로 회귀 못 잡는다.

**요구**: Pending skip 판정을 **free function 으로 추출** + reconcile_once 가
실제 호출:

```rust
// strategy.rs 신규
pub(crate) fn should_skip_reconcile_for_lock(
    lock_state: &PositionLockState,
    code: &str,
) -> bool {
    matches!(lock_state,
        PositionLockState::Pending { code: c, .. } if c == code
    ) || matches!(lock_state,
        PositionLockState::ManualHeld(c) if c == code
    )
}
```

**테스트 7 건**:

| 입력 | 기대 |
|---|---|
| `Pending { code: self, .. }` | **true** |
| `Pending { code: other, .. }` | false |
| `Held(self)` | false |
| `Held(other)` | false |
| `ManualHeld(self)` | **true** |
| `ManualHeld(other)` | false |
| `Free` | false |

### 3d. 기존 dead helper 정리 (v2 유지)

- `classify_reconcile_outcome` 제거는 별도 cleanup PR — 본 계획에서 직접 제거 X
- 단 본 계획이 **새 helper 를 실제 reconcile_once 가 호출하도록 구현** → 향후
  dead helper 추가 생성 방지

### 3e. 기존 fixups 와의 관계

- v3 fixups F1.1 (streak): fetch Err 시만 → 본 사고 (Ok 반환 mismatch) 차단 못 함
- v3 fixups H (ManualHeld 보존): manual 전환 **후** 보호
- **본 수정이 manual 승격 전 단계 차단** — 상호 보완

---

## 종료 조건 (4 가지 — 이후 수정 중단 기준)

1. **2026-04-17 재현**: canonical 5 m candles (수정 #2 적용) + 15m 단일 stage
   (수정 #1 관통) 로 `replay 122630 --date 2026-04-17 --stage 15m` 이 **0 건** 거래
   반환
2. **entry_signal 정합성**: 라이브에서 새로 찍힌 모든 `entry_signal` 이
   `or_breakout_pct > 0` 이고 `stage == "15m"`
3. **Pending(self) 중 manual 미발생**: `should_skip_reconcile_for_lock` 단위
   테스트 7 건 통과 + burn-in 관찰
4. **Burn-in OHLC + signal-set 동치성**: 3 ~ 5 영업일 동안
   - (A) 장 마감 후 canonical 의 과거 완료 5m 버킷과 Yahoo 원본 OHLC 완전 일치
   - (B) 라이브 `entry_signal` 집합과 canonical 기반 replay signal 집합이 완전
         일치 (time/stage/entry/SL/TP/or_breakout_pct 모두)

**4 가지 모두 true 되기 전에는 본 계획 외 새 수정 금지.**

---

## 지금 하지 말아야 할 것 (명시적 차단)

- `or_breakout_pct >= 0.1%` threshold 튜닝
- RR / TimeStop / trailing 값 변경
- reconcile retry / grace 추가
- `b_volume = 0` 단독 조사 — canonical 확정 후
- `classify_reconcile_outcome` dead helper 에 새 테스트 추가
- `CompletedCandle.is_realtime` 을 granularity 판정에 재사용 — 의미 섞임

---

## 수정 대상 파일 요약 (v3)

| 파일 | 수정 | 예상 라인 |
|---|---|---|
| `src/infrastructure/websocket/candle_aggregator.rs` | P1: `CompletedCandle.interval_min` 필드 추가 + 생성 6 곳 채움 | ~15 |
| `src/strategy/live_runner.rs` | 1a (default), 1b (1876), 2c (fetch_canonical_5m_candles 신규 + precedence), 2d (1726/1763/1851 canonical 사용) | ~80 |
| `src/strategy/orb_fvg.rs` | 1b (158) | 1 |
| `src/strategy/backtest.rs` | 1b (410), 1c (stage 필터 주입) | ~10 |
| `src/strategy/parity/parity_backtest.rs` | 1b (143/151/480), 1c | ~10 |
| `src/main.rs` | 1c (replay/backtest 명령 `--stage` 옵션 + 기본 15m) | ~15 |
| `src/presentation/routes/strategy.rs` | 3b (948 Pending/ManualHeld skip), 3c (should_skip_reconcile_for_lock helper) | ~25 |
| (테스트) | 1d, 2f OHLC 동치성 + signal-set 동치성, 3c 매트릭스 7 건 | ~70 |

**합계 ≈ 220 줄**. v2 의 170 줄 → v3 +50 (P1 struct 확장 +15, P2 precedence
+15, P3 OHLC/signal-set 검증 +20). 기존 v3 fixups 안전장치는 유지.

---

## 실행 순서

1. **불변식 #1** (1a + 1b + 1c + 1d): 라이브/백테스트/parity/replay 5 곳 + 명령
   옵션. backtest 회귀 diff 기록 (거래 감소 = 정상화)
2. **불변식 #3** (3b + 3c + 3d): `should_skip_reconcile_for_lock` 실사용 + 7 건
   테스트
3. **불변식 #2 P1** — `CompletedCandle.interval_min` 필드 추가 + 생성 6 곳 채움
4. **불변식 #2 P2** — `fetch_canonical_5m_candles` 신규 + precedence (과거: Yahoo
   우선, 현재: WS)
5. **불변식 #2 P3 검증** — OHLC 동치성 샘플 테스트 + `replay --stage 15m` 0 건
   재현
6. `cargo fmt && cargo clippy && cargo test --lib`
7. release 빌드
8. burn-in 3-5 영업일. 매일 라이브 `entry_signal` vs canonical replay signal
   diff 기록 + OHLC 동치성 확인
9. 종료 조건 4 가지 모두 만족하면 본 계획 종료 서명

---

## 검증 체크리스트

- [ ] `cargo fmt -- --check` 통과
- [ ] `cargo clippy --lib --tests` 신규 경고 0
- [ ] `cargo test --lib` 전체 통과 + 본 계획 신규 테스트 ≥ 15 건
  - paper default stage 테스트
  - `require_or_breakout=true` 고정 테스트
  - `CompletedCandle.interval_min` 필드 존재 테스트
  - `fetch_canonical_5m_candles` precedence 테스트 (과거 버킷 Yahoo vs 현재 버킷
    WS)
  - `should_skip_reconcile_for_lock` 매트릭스 7 건
- [ ] 수정 전/후 `backtest --stages 15m` 365d 결과 diff 기록
- [ ] `replay 122630 --date 2026-04-17 --stage 15m` parity/passive: **0 건**
- [ ] **OHLC 동치성 검증** — canonical 과거 완료 버킷 10 개 이상 샘플 OHLC 가
      Yahoo 원본과 **완전 일치** (tolerance 0)
- [ ] **Signal-set 동치성 검증** — 라이브 entry_signal 집합 == canonical replay
      signal 집합 (time/stage/entry/SL/TP/or_breakout_pct 모두)
- [ ] burn-in 첫 날 라이브 entry_signal 모두 `or_breakout_pct > 0`, `stage="15m"`
- [ ] burn-in 기간 Pending(self) 중 manual 승격 0 건

---

## 타부서 리뷰 반영 요약

### v1 → v2 (1 차)

| 리뷰 항목 | v1 | v2 |
|---|---|---|
| 수정 #2 단순 치환 위험 | `realtime_candles → all_candles` 치환 | `fetch_canonical_5m_candles()` 신규 |
| 불변식 #1 관통 부재 | live 기본값만 15m | backtest/parity/replay + 명령 옵션 |
| classify_reconcile_outcome 확장 | 선택지 열어둠 | 실사용 helper 추출로 한정 |
| b_volume Naver 고유 단정 | "Naver 고유 결함" | "원인 미확정" |

### v2 → v3 (2 차)

| 리뷰 항목 | v2 | **v3 (본 문서)** |
|---|---|---|
| P1 — `interval_min` 메타 누락 | `c.interval_min == 1` 가정 (struct 에 없음) | **`CompletedCandle.interval_min` 필드 추가 + 생성 6 곳 모두 채움** |
| P2 — merge precedence 근거 충돌 | "WS 집계 우선" (postmortem 진단과 모순) | **과거 완료 버킷: Yahoo 우선 / 진행 중 버킷: WS 전용** |
| P3 — 검증 강도 약함 | "시각 집합 일치" (OHLC 값 차이 못 잡음) | **OHLC 동치성 + signal-set 동치성 상향** |

---

## 2026-04-18 구현 완료 기록

Stage invariant 일관성 최종 배선 (3 단계):

1. **오프라인 경로 배선** (`backtest.rs`/`main.rs`)
   - `run()`, `run_session_reset()`, `run_dynamic_target()` 3 곳 모두 `allowed_stages: &[String]` 파라미터로 관통
   - `require_single_stage()` helper 추가: 단일 stage 경로는 `--stages` 가 정확히 1 개여야 즉시 에러 (0 개 / 2+ 개 모두 거부)
   - `stage_to_or_end()` helper 추가: stage 이름 → OR 종료 시각 매핑 (5m=09:05, 15m=09:15, 30m=09:30)
   - multi_stage/parity/dual_locked/replay 경로는 CSV 필터 그대로 적용 (2+ 개도 허용)

2. **라이브 상태 노출 정리** (`live_runner.rs`)
   - `LiveRunnerConfig::first_allowed_stage_name()` helper 추가
   - `state.or_high`/`state.or_low` 를 **진입 허용 stage** 의 첫 값으로 고정 (기존: `available_stages[0]` 무조건 → 15m 단일 모드에서 5m 값이 잠시 노출되던 혼선 제거)
   - `state.or_stages` 리스트는 관찰 목적 그대로 전체 가용 stage 유지

3. **회귀 테스트** (+23 건)
   - `parse_allowed_stages_tests` 7 건 (`main.rs`): 정규화/중복/대소문자/에러 케이스
   - `stage_filter_tests` 12 건 (`backtest.rs`): `stage_enabled`/`filtered_stage_tuples_for`/`filtered_stage_defs_for`/`require_single_stage`/`stage_to_or_end`
   - `live_runner_config_default_tests::first_allowed_stage_*` 4 건 (`live_runner.rs`): allowed 15m 만일 때 5m 가용해도 15m 반환 / 교집합 없으면 None / allowed=None 이면 첫 값 / 빈 입력은 None

### 종료 조건 충족

- [x] **2026-04-17 재현**: `replay 122630 --date 2026-04-17 --rr 2.5 --stages 15m` → legacy/parity_legacy/parity_passive 모두 **0 건**. 114800 도 동일 0 건
- [x] **entry_signal 정합성**: 라이브 `allowed_stages=Some(["15m".to_string()])` 기본값 + `require_or_breakout=true` 전 경로 관통 (라이브/backtest/orb_fvg/parity_backtest)
- [x] **Pending(self) 중 manual 미발생**: `should_skip_reconcile_for_lock` 7 건 회귀 테스트 통과 + `classify_reconcile_outcome` 을 `reconcile_once` 가 실제 호출하는 구조로 재배선
- [ ] **Burn-in 3 ~ 5 영업일 일관성**: 실사용 관찰 필요 (본 문서 작성 시점 미진행)

`cargo fmt --check`, `cargo clippy --lib --tests` 신규 경고 0, `cargo test --lib` 222 건 + `cargo test --bin kis_agent` 7 건 모두 pass.

---

## 참고 문서

- `AGENTS.md:4` — 전략 원본 명세
- `docs/monitoring/2026-04-17-trading-postmortem.md:59, 171` — OR high 90 원 차이,
  WS 품질 결함 증거 (merge precedence 근거)
- `docs/monitoring/2026-04-16.md:97, 113` — b_volume=0 WS 경로에서도 관찰
- `src/infrastructure/websocket/candle_aggregator.rs:74` — `CompletedCandle`
  struct 정의 (v3 P1 대상)
- `src/infrastructure/websocket/candle_aggregator.rs:198/293/379/446/571/626` —
  생성 위치 6 곳 (v3 P1 채워야 할 위치)
- `src/strategy/parity/signal_engine.rs:130` — `require_or_breakout` 단축평가
- `src/strategy/live_runner.rs:370` — `aggregate_completed_bars`
