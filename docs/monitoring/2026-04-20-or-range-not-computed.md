# 2026-04-20 OR 범위 미계산 사고 — preflight gate 와 OR 계산 결합 결함

## 제목 변경 경위

최초 Task 명은 **"5분봉 DB 적재 실패"** 였으나, 코드 추적 결과 5분봉 DB 적재는
**의도된 설계상 장중에는 수행되지 않는다** 는 점을 확인했다. 진짜 원인은
`poll_and_enter` 내부에서 **preflight gate 실패 시 early return 이 OR 계산 코드
를 함께 skip** 하는 것이다. 따라서 문서 제목을 사고의 실제 근본 원인으로 교체했다.

## 요약 (TL;DR)

- **증상**: 2026-04-20 10:14 시점 두 종목 모두 `or_high/or_low=null`, `or_stages=[]`.
  웹 status API 응답에 `or_refresh_warnings=["5m","15m","30m"]` 표시.
- **근본 원인**: `live_runner.rs::poll_and_enter` (line 4920~5085) 에서 preflight
  gate 실패 시 **line 4957 에서 early return** 하여, 동일 함수의 뒤쪽에 있는
  **OR 계산 블록 (line 4967~5085)** 이 한 번도 실행되지 않음.
- **촉발 조건**: 장 시작 5분 후 (09:05:00) `spread_ok=false` 로 Degraded 전입 →
  poll 루프 매 iteration 마다 preflight 실패 → OR 계산까지 도달 못 함 → 전체
  장시간 `or_high/or_low` 가 `None` 으로 고정.
- **영향**: 오늘 신규 진입 물리적 불가. OR 없으면 FVG 신호 해석이 성립하지 않음.
- **심각도**: Critical — 설계 결함. gate/계산 경계 분리 (계획서 §2-2) 위반.
- **권장 조치 우선순위**:
  1. OR 계산을 preflight gate 앞으로 이동, 또는 `compute_or_if_ready()` 로 분리해
     gate 와 독립 실행.
  2. Degraded 해제 경로 (`clear_degraded_if_healthy`) 가 gate 재검증 기반으로만
     동작하는지 재확인 — 동적 복구 가능 여부.
  3. `or_refresh_warnings` 의미 재정리 (문서상 의미와 실제 운영 혼선).

## 관측된 증거

### Status API (10:14:53 KST 시점)

```json
[
  {
    "code": "122630",
    "name": "KODEX 레버리지",
    "active": true,
    "state": "신호 탐색",
    "today_trades": 0,
    "today_pnl": 0.0,
    "message": "degraded: spread_exceeded",
    "or_high": null,
    "or_low": null,
    "or_stages": [],
    "or_source": "ws",
    "or_refresh_warnings": ["5m", "15m", "30m"],
    "degraded": true,
    "manual_intervention_required": false,
    "degraded_reason": "spread_exceeded"
  },
  { "code": "114800", ... 위와 동일 패턴 ... }
]
```

### DB 캔들 적재 상태

| 항목 | 122630 | 114800 |
|---|---|---|
| `minute_ohlcv` interval_min=1 (오늘) | 75 건, last=10:14:00 | 75 건, last=10:14:00 |
| `minute_ohlcv` interval_min=5 (오늘) | **0 건** | **0 건** |
| `daily_or_range` (오늘) | **0 건** | **0 건** |

→ **1분봉은 WS 로 정상 수신** (75건/종목, 10:14:00 까지 거의 매분 수신).
→ **5분봉 interval_min=5 가 0건** 인 이유는 아래 §"5분봉 DB 적재는 의도된 비저장"
  참고 (오해였음).
→ **`daily_or_range` 도 0건** — OR 계산 자체가 한 번도 실행되지 않았다는 증거.

### event_log 기록 분포 (오늘)

| event_type | category | severity | 건수 | 최초 | 최후 |
|---|---|---|---|---|---|
| phase_change | strategy | info | 6 | 04:11:50 | 09:05:00 |
| ws_reconnect | system | warn | 154 | 04:13:26 | 10:00:00 |
| api_error | system | warn | 31 | 07:46:00 | 10:12:00 |
| **preflight_gate_blocked** | strategy | warn | **1720** | **09:05:00** | **10:16:40** |
| transition | execution_state | info | 2 | 09:05:00 | 09:05:00 |

→ **1720 건의 preflight_gate_blocked** 가 모두 `spread_exceeded` 사유로 적재됨.
→ 09:05:00 에 Degraded 전이 시점을 명확히 보여줌 (transition 2건 = 두 종목).

### 로그 상 분봉 집계 시점

```
2026-04-20 09:00:11 KST   분봉 집계 시작: 114800 (090011)
2026-04-20 09:00:14 KST   분봉 집계 시작: 122630 (090014)
```

- "분봉 집계 시작" 은 종목별 첫 tick 수신 시에만 찍히므로 2건이 정상.
- "분봉 완성" 은 `debug!` 레벨이라 info 로그에는 안 나오지만, DB 에 75건씩
  적재된 것으로 보아 실제 완성/저장은 정상 작동 중.

## 코드 추적

### 핵심 결함 지점

`src/strategy/live_runner.rs::poll_and_enter`:

```text
line 4920    // [Phase A] preflight gate 검증
line 4923~4958  if !preflight.all_ok() {
line 4938        self.transition_to_degraded(reason).await;
line 4957        return Ok(false);                  ← early return
             }
line 4960    self.clear_degraded_if_healthy().await;
line 4962    if !preflight.allows_entry_for(...) { return Ok(false); }

line 4967    // [Phase B] OR 계산 시작 — preflight 통과해야만 도달
line 4970    let canonical_candles = self.fetch_canonical_5m_candles().await?;
line 4981~5055  OR 단계별 계산 (5m / 15m / 30m)
line 5068~5085  state.or_high = ... / state.or_low = ... / state.or_stages = ...

line 5094~    // [Phase C] FVG 탐색 + 진입
```

**문제**: Phase A (gate) 가 실패하면 Phase B (OR 계산) 까지 전체 skip.
Phase B 의 결과는 UI/웹 API 가 `or_high`/`or_low`/`or_stages` 로 노출하는 값이므로,
Phase A 가 통과하지 못하면 이 세 값은 계속 `None` 으로 남는다.

### 5분봉 DB 적재는 의도된 비저장 (오해)

`candle_aggregator.rs` 의 DB 저장 경로는 항상 `interval_min: 1`:

- `save_candle_to_db()` (line 762): 하드코딩 `interval_min: 1`
- 네이버 백필 (line 511): 하드코딩 `interval_min: 1`

반면 Yahoo 백필 (`collector::yahoo_minute`) 은 `interval_min: 5` 로 저장하나,
장중에는 Yahoo 당일 데이터가 사실상 가용하지 않음 (오전 기동 시 전 거래일분만
수신). 따라서 **장중 DB 에 `interval_min=5` 가 0 건인 것은 정상**.

### OR 계산은 메모리 기반 (DB 독립)

`live_runner.rs::fetch_canonical_5m_candles` (line 9131~9196):

1. `ws_candles` Arc<RwLock<HashMap<_, Vec<CompletedCandle>>>> 에서 수집된 1분봉을 읽음
2. `aggregate_completed_bars(&ws_1m, 5, now_time)` 로 메모리상 5분봉 집계
3. Yahoo/Naver 백필이 있으면 `interval_min=5` 로 별도 병합

즉 OR 계산 자체는 DB 의존이 아니라 `ws_candles` 메모리 의존. 오늘 1분봉이 정상
수신됐으므로 **메모리에는 5분봉 집계 재료가 충분**했다. 만약 `poll_and_enter`
가 Phase B 까지 도달했다면 OR 는 계산되었을 것이다.

### `or_refresh_warnings` 의 실제 의미

`presentation/routes/strategy.rs:1384~1391`:

```rust
if let Some(ref agg) = state.strategy_manager.candle_aggregator {
    let failures = agg.or_refresh_failures().await;
    for status in list.iter_mut() {
        status.or_source = agg.effective_source(&status.code).await;
        status.or_refresh_warnings = failures.clone();
    }
}
```

`or_refresh_warnings` 는 **`CandleAggregator` 의 OR 백필 (Yahoo/Naver) 실패
stage 목록** — OR **계산** 실패 표시가 아니다. `["5m","15m","30m"]` 은 "백필
소스가 실패했다" 는 사실만 의미하며, 장중 Yahoo 당일 데이터 부재 상황에서는
정상적으로 3 stage 모두 warning 이 뜰 수 있다. 이 필드 이름이 "OR 계산 실패"
를 암시해 진단 초기에 혼선을 유발했다.

## 왜 오늘 spread_ok=false 가 지속됐나

`refresh_preflight_metadata` (live_runner.rs:4269~4277):

```rust
let internal_spread_ok = self
    .latest_quote_snapshot()
    .await
    .map(|(ask, bid)| {
        let mid = ((ask + bid) / 2).max(1);
        let tick = etf_tick_size(mid);
        ask >= bid && (ask - bid) <= tick * 3   // ← 3 tick 이하만 허용
    })
    .unwrap_or(!self.live_cfg.real_mode);
```

- 임계치: **3 틱 이하**.
- 위반 시 spread_exceeded. 이 조건이 오늘 09:05~10:16 구간에 지속적으로 불만족.
- 자세한 분석은 자매 문서 [2026-04-20-spread-exceeded-early-degraded.md](./2026-04-20-spread-exceeded-early-degraded.md).

## Degraded 해제 경로는 작동 중인가

`live_runner.rs::clear_degraded_if_healthy` 는 `poll_and_enter` 의 Phase A 끝
(line 4960) 에 있다. 하지만 이 코드는 **`preflight.all_ok()` 가 true 인 경우에만**
도달. 즉 "이미 preflight 가 복구된 iteration" 에서만 Degraded 해제 시도.

오늘의 실제 흐름:
1. 매 iteration: preflight 재계산 → `spread_ok=false`
2. `!preflight.all_ok()` → transition_to_degraded("spread_exceeded") + return
3. clear_degraded_if_healthy 에 도달 못 함

→ **Degraded 해제 경로는 존재하나, gate 자체가 복구되어야만 호출되는 구조**.
gate 가 장시간 실패하면 Degraded stuck.

## 영향

- 오늘 (2026-04-20 월) KODEX 레버리지 (122630) / KODEX 인버스 (114800) **진입 0**.
- `trades` 테이블 0건, `active_positions` 0건 — 모의투자 자본 증감 없음 (단, 실전에서
  같은 결함이 발생하면 실제 기회 손실).
- OR 수집 시간대 (09:00~09:15) 이미 지났으므로 오늘 재개해도 복구 불가.

## 권장 수정 방향

### 1. OR 계산을 gate 앞으로 이동 (최우선)

`poll_and_enter` 구조를 다음과 같이 재정렬:

```rust
// 1. OR 계산 먼저 (gate 와 독립)
let or_stages = self.refresh_or_stages().await?;
{
    let mut state = self.state.write().await;
    state.or_high = ...;
    state.or_low = ...;
    state.or_stages = or_stages.clone();
}

// 2. preflight gate (신규 진입 허용 여부만 판정)
let preflight = self.refresh_preflight_metadata().await;
if !preflight.all_ok() {
    self.transition_to_degraded(...).await;
    return Ok(false);  // ← OR 은 이미 계산됨
}
self.clear_degraded_if_healthy().await;

// 3. FVG 탐색 + 진입
...
```

이렇게 하면 Degraded 상태에서도 OR 은 UI/웹 API 에 정상 노출되고, Degraded
해제 시점에 FVG 탐색 즉시 가능.

### 2. compute_or_if_ready() 분리 (계획서 원칙 준수)

실행 타이밍 계획서 §2-2 "OR 계산은 전략 계층의 signal 경로, 진입은 실행 계층의
gate 경로" 원칙에 따라:

- `strategy::signal_engine::compute_or_stages(candles, now, cfg)` — pure fn
- `strategy::live_runner` 는 OR 계산 결과를 소비만 (결정하지 않음)
- preflight gate 는 진입 허용/차단만 결정

이 분리는 PR #4~5 의 actor 전이 로드맵과 자연스럽게 맞물린다.

### 3. `or_refresh_warnings` 필드 의미 명확화

현 의미는 "Yahoo/Naver 백필 실패 stage" 인데 이름이 "refresh_warnings" 이라
오해 여지가 크다. 옵션:

- 필드명 변경: `or_backfill_failures: Vec<String>`
- 또는 별도 필드 추가: `or_compute_ready: bool` / `or_last_compute_error: Option<String>`
  로 "계산 수행 여부" 를 명시

### 4. Degraded stuck 감시 알림 (선택)

gate 가 `N분 이상 연속 실패` 면 `event_log.degraded_stuck` 같은 severity=critical
이벤트 1회 기록 + operator 알림. 현재는 `preflight_gate_blocked` 1720건이 노이즈
처럼 쌓여 "이게 정상 폴링 로그인지 실제 stuck 인지" 구분이 어렵다.

## 재발 방지 테스트 아이디어

1. **Unit**: `poll_and_enter` 의 공개 테스트 helper 를 만들어 "preflight 실패여도
   `state.or_high` 는 업데이트된다" 를 검증. 현재는 전체 함수가 비공개 async 라 테스트
   불가 → 리팩터링 이후 가능.
2. **Integration**: 모의 mode 에서 spread 를 인위적으로 3틱 초과로 주입 (호가 stub)
   하고 09:15 까지 돌렸을 때 `status.or_high != null` 인지 assert.
3. **Parity**: 백테스트 경로 (`strategy::backtest::run_*`) 와 라이브 OR 계산이
   동일한 pure fn 을 호출하는지 확인 — 지금은 pure fn 추출이 안 돼있어 parity 리스크.

## 관련 파일 · 참고

- `src/strategy/live_runner.rs:4920~5085` — poll_and_enter 의 gate + OR 블록
- `src/strategy/live_runner.rs:4259~4340` — refresh_preflight_metadata
- `src/strategy/live_runner.rs:9131~9196` — fetch_canonical_5m_candles (메모리 기반)
- `src/infrastructure/websocket/candle_aggregator.rs:540~679` — 1분봉 tick → memory + DB
- `src/infrastructure/websocket/candle_aggregator.rs:744~767` — save_candle_to_db (interval_min: 1 하드코딩)
- `src/presentation/routes/strategy.rs:1376~1394` — get_status (or_refresh_warnings 출처)
- `docs/monitoring/2026-04-18-execution-timing-implementation-plan.md` §2-2 — 계산/실행 분리 원칙
- `docs/monitoring/2026-04-19-execution-followup-plan.md` §5 — 후속 actor 로드맵
- 자매 문서: [2026-04-20-spread-exceeded-early-degraded.md](./2026-04-20-spread-exceeded-early-degraded.md)

## 다음 거래일 (4/21 화) 전 처리 여부

- **권장**: 위 §"권장 수정 방향" #1 (OR 계산을 gate 앞으로) 만이라도 적용.
  이건 국지적 재배열이라 리스크 낮다.
- **자매 이슈**: spread 임계치 조정은 별도 판단 필요 (자매 문서 참고).
- 두 개선 없이 4/21 재개하면 **동일 사고 재발 확률 매우 높음** (모의 호가 특성상
  spread > 3 tick 이 장 시작 직후 빈번).
