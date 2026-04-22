# 2026-04-20 spread_exceeded 조기 Degraded — tick size 오산 + metadata 실측치 미기록

## 요약 (TL;DR)

- **증상**: 2026-04-20 09:05:00 에 KODEX 레버리지 (122630) + KODEX 인버스 (114800)
  두 종목 모두 `spread_exceeded` 사유로 Degraded 상태에 빠진 뒤, 10:16 까지 약
  1 시간 11 분 동안 preflight_gate_blocked 이벤트가 **1720 건** 누적되며 복구
  되지 않음.
- **치명적 근본 원인**: `live_runner.rs::etf_tick_size` 함수가 KRX ETF 호가단위
  규칙을 잘못 반영. `price >= 5000` 에서 일괄 **5원** 반환하는데, 실제 KRX ETF
  호가단위는 가격대별로 1원 / 5원 / **50원** 으로 달라진다. 특히 **50,000원 이상
  은 50원**. KODEX 레버리지는 101,915원 구간이라 실제 최소 스프레드가 50원이지만
  코드가 15원 (5×3) 으로 비교하므로 **거의 항상 spread_exceeded**.
- **보조 원인**: `preflight_gate_blocked` 이벤트 metadata 에 실제 ask/bid/spread
  수치가 기록되지 않아, **임계치와 실측치의 관계를 사후 분석 불가**. 사고 당시
  "정말 스프레드가 넓었나, 아니면 임계치가 잘못됐나" 를 DB 기록만으로 판별
  불가능했다.
- **연쇄 효과**: 자매 문서 [2026-04-20-or-range-not-computed.md](./2026-04-20-or-range-not-computed.md)
  참고 — preflight gate 실패가 OR 계산 skip 까지 연쇄시켜 오늘 진입 0.
- **심각도**: Critical — KRX 가격 규칙 하드코딩 오류. 5만원 이상 ETF 는 실전/모의
  구분 없이 모두 같은 증상.
- **권장 조치**:
  1. `etf_tick_size` 를 KRX ETF 호가단위 표에 맞게 교정 (최소 50,000원 이상 구간 50원 분기).
  2. `preflight_gate_blocked` metadata 에 `ask`, `bid`, `mid`, `spread`, `tick`,
     `threshold_ticks`, `spread_in_ticks` 기록 추가.
  3. 임계치 `tick * 3` 의 적정성 재검토 (모의투자 환경 스프레드 실측 후).

## 관측된 증거

### Degraded 진입 순간

```
2026-04-20 09:05:00.056 KST  KODEX 레버리지: execution_state flat → degraded (spread_exceeded)
2026-04-20 09:05:00.056 KST  KODEX 레버리지: Degraded 진입 — spread_exceeded
2026-04-20 09:05:00.290 KST  KODEX 인버스: execution_state flat → degraded (spread_exceeded)
2026-04-20 09:05:00.290 KST  KODEX 인버스: Degraded 진입 — spread_exceeded
```

- 두 종목이 0.23 초 차로 연속 Degraded — 두 runner 가 동일한 polling interval 로
  돌기 때문.
- 장 시작 (09:00) 후 정확히 5 분 경과 시점.

### preflight_gate_blocked metadata 샘플

```json
{
  "reason": "spread_exceeded",
  "spread_ok": false,
  "nav_gate_ok": true,
  "regime_confirmed": true,
  "active_instrument": "122630",
  "subscription_health_ok": true,
  "entry_cutoff": "15:20:00"
}
```

→ 세 번의 샘플 모두 동일 구조. **실제 ask/bid/spread 수치가 metadata 에 없다.**
→ 필드명에 "`spread_ok: false`" 만 있고 실측 수치가 없어 임계치 조정 판단 불가.

### 이벤트 빈도

```
event_type: preflight_gate_blocked
첫 발생:   09:05:00.056
최근 발생: 10:16:40.808
총 건수:   1720건
```

→ 1 시간 11 분 동안 약 14 회/분. poll_and_enter 루프가 5초 폴링 + 두 종목이라
  이론상 24 회/분. 실제 14 회는 일부 iteration 이 다른 경로 (armed 등) 로 진입
  했거나 WS 재연결 중 pause 때문일 가능성.
→ 이 숫자가 **Degraded 해제가 한 번도 안 됐다는 간접 증거**.

## 코드 추적

### 버그 핵심: `etf_tick_size`

`src/strategy/live_runner.rs:9762~9768`:

```rust
fn etf_tick_size(price: i64) -> i64 {
    if price < 5_000 {
        1
    } else {
        5
    }
}
```

**문제**: 이 함수는 `price >= 5000` 전부를 5원으로 보지만, KRX ETF 호가단위는
가격대별로 계단식이다. 실제 규칙 (2024 개정 기준):

| 가격대 | KRX ETF 호가단위 | 현재 코드 반환 | 일치? |
|---|---|---|---|
| 2,000원 미만 | 1원 | 1원 | ✅ |
| 2,000 ~ 5,000원 | 5원 | 1원 | ❌ (실측 5원인데 1원 반환) |
| 5,000 ~ 50,000원 | 5원 | 5원 | ✅ |
| **50,000원 이상** | **50원** | **5원** | ❌ **10배 과소** |

122630 (KODEX 레버리지) 의 10:14 시점 가격: **101,915원**.
- 실제 KRX 1호가 스프레드 = 50원 (호가 단위 자체).
- `etf_tick_size(101915) = 5` — 틀림.
- `spread_ok` 판정 임계: `tick * 3 = 15원` — 틀림.
- 정상 시장 스프레드 (50원) >> 임계 (15원) → **항상 spread_exceeded**.

→ **122630 은 구조적으로 개장 순간부터 절대 gate 통과 불가**.

### 114800 (KODEX 인버스) 의 경우

10:14 시점 가격: **1,439원** (`price < 5000`).
- 실제 KRX 1호가 스프레드 = 1원.
- `etf_tick_size(1439) = 1`, `threshold = 3원` — 정상.
- 일반 시장 상황에서 spread 는 1~2원 → 통과.
- 09:05:00 에 spread_exceeded 로 빠진 이유: 장 시작 5분 구간 호가 변동성이
  가장 높은 시점이라 ask-bid 가 순간 3원 초과했을 가능성. 다만 metadata 에
  실측치가 없어 확정 불가.
- 이후 Degraded 복구가 안 되는 이유는 "한 번 Degraded 진입한 뒤 다시 통과하는
  iteration 이 없었다" 뿐. 시장 스프레드가 종일 3원을 초과한다고 보기 어려우나
  순간 폭이 큰 tick 이 계속 잡혔을 가능성 있음 (자매 이슈 참고).

### spread_ok 계산 경로

`live_runner.rs:4269~4277`:

```rust
let internal_spread_ok = self
    .latest_quote_snapshot()
    .await
    .map(|(ask, bid)| {
        let mid = ((ask + bid) / 2).max(1);
        let tick = etf_tick_size(mid);
        ask >= bid && (ask - bid) <= tick * 3
    })
    .unwrap_or(!self.live_cfg.real_mode);
```

- `ask >= bid` — 기본 건전성.
- `(ask - bid) <= tick * 3` — **3 tick** 이하.
- 기본값 (quote snapshot 없음): paper mode 에서는 `true`, real mode 에서는 `false`
  (보수적). paper 기본값 true 는 타당하나, **오늘은 latest_quote_snapshot 이 실제로
  존재했기 때문에 tick 오산이 효력을 발휘**.

### metadata 기록 지점

`live_runner.rs:4940~4955` (log_event 호출):

```rust
el.log_event(
    self.stock_code.as_str(),
    "strategy",
    "preflight_gate_blocked",
    "warn",
    &format!("preflight gate 실패 ({reason}) — 신규 진입 차단"),
    serde_json::json!({
        "reason": reason,
        "spread_ok": preflight.spread_ok,
        "nav_gate_ok": preflight.nav_gate_ok,
        "regime_confirmed": preflight.regime_confirmed,
        "subscription_health_ok": preflight.subscription_health_ok,
        "entry_cutoff": preflight.entry_cutoff.map(|v| v.to_string()),
        "active_instrument": preflight.active_instrument,
    }),
);
```

→ **실제 ask/bid/mid/tick/threshold 가 전혀 기록되지 않음**.
→ `PreflightMetadata` 구조체 (live_runner.rs 823 근처) 에도 spread 원값이 없음
  — 계산 후 bool 만 남기고 원값은 버려짐.

## Degraded 복구 경로

### 현재 구조

`clear_degraded_if_healthy` 는 `poll_and_enter` Phase A (line 4960) 에 위치.
preflight.all_ok() 가 true 인 iteration 에서만 호출된다 (자매 문서 §"Degraded
해제 경로" 참고).

오늘 관측:
- 09:05 이후 모든 iteration 에서 `preflight.all_ok() = false` → transition_to_degraded
  + early return.
- clear_degraded_if_healthy 에 한 번도 도달하지 못함.
- tick size 오산이 고쳐지지 않는 한 122630 은 **개장부터 마감까지 영원히 Degraded**.

### Degraded stuck 모니터링 부재

`event_log` 에 `preflight_gate_blocked` 이 초당 수차례 쌓이지만, "이게 정상적인
polling 로그인지 실제 stuck 인지" 구별할 meta signal 이 없다. 1720건이 쌓였는데도
별도 경보 이벤트 (`degraded_stuck`, severity=critical) 는 발행되지 않음.

## 영향

- 오늘 122630 진입 0건 (tick 버그로 구조적 불가).
- 오늘 114800 진입 0건 (순간 spread 초과 + 복구 경로 불재구조).
- `trades` 0건, `active_positions` 0건 — 모의 손익 0.
- 더 심각: **실전 전환 시 동일 증상 발생**. tick 오산은 paper/real 무관.

## 권장 수정

### 1. etf_tick_size 교정 (즉시 필요, 한 줄급 수정)

KRX ETF 호가단위 표 그대로 반영:

```rust
fn etf_tick_size(price: i64) -> i64 {
    // KRX ETF 호가단위 (2024 개정).
    // 2,000 미만 = 1원
    // 2,000 ~ 50,000 = 5원
    // 50,000 이상 = 50원
    if price < 2_000 {
        1
    } else if price < 50_000 {
        5
    } else {
        50
    }
}
```

단위 테스트도 같이:

```rust
#[test]
fn etf_tick_size_matches_krx_table() {
    assert_eq!(etf_tick_size(1_439), 1);    // KODEX 인버스
    assert_eq!(etf_tick_size(4_999), 5);    // 2,000 ~ 5,000 경계
    assert_eq!(etf_tick_size(49_999), 5);   // 5,000 ~ 50,000
    assert_eq!(etf_tick_size(50_000), 50);  // 경계
    assert_eq!(etf_tick_size(101_915), 50); // KODEX 레버리지
}
```

`round_to_etf_tick` 도 이 함수를 사용하므로 정정하면 주문가격 round 도 동시에 정정됨.

### 2. preflight metadata 에 실측치 기록 (진단 가능성 복구)

`refresh_preflight_metadata` 에서 spread_ok 계산 시 실측값을 `PreflightMetadata` 에 보존:

```rust
pub struct PreflightMetadata {
    // 기존 필드 ...
    pub spread_ok: bool,
    // 2026-04-20 추가: 진단용 실측치
    pub spread_ask: Option<i64>,
    pub spread_bid: Option<i64>,
    pub spread_raw: Option<i64>,
    pub spread_tick: Option<i64>,
    pub spread_threshold_ticks: u32,  // 현재 3 고정, 추후 config
}
```

그리고 `preflight_gate_blocked` log_event metadata 에 이 필드들 포함:

```rust
serde_json::json!({
    "reason": reason,
    "spread_ok": preflight.spread_ok,
    "spread_ask": preflight.spread_ask,
    "spread_bid": preflight.spread_bid,
    "spread_raw": preflight.spread_raw,
    "spread_tick": preflight.spread_tick,
    "spread_in_ticks": preflight.spread_raw
        .zip(preflight.spread_tick)
        .map(|(r, t)| r as f64 / t as f64),
    "spread_threshold_ticks": preflight.spread_threshold_ticks,
    ...
})
```

이렇게 하면 다음 사고에서 "실제 몇 틱이었는지" 를 SQL 로 즉시 추출 가능:

```sql
SELECT event_time::time AS t,
       stock_code,
       (metadata->>'spread_raw')::bigint AS spread,
       (metadata->>'spread_tick')::bigint AS tick,
       (metadata->>'spread_in_ticks')::float AS ticks,
       (metadata->>'spread_threshold_ticks')::int AS threshold
FROM event_log
WHERE event_type = 'preflight_gate_blocked'
  AND DATE(event_time AT TIME ZONE 'Asia/Seoul') = CURRENT_DATE
ORDER BY event_time DESC
LIMIT 20;
```

### 3. 임계치 상수화 + config

현 `tick * 3` 은 hardcoded magic number. 다음 중 하나로 이동:

- `LiveRunnerConfig::spread_threshold_ticks: u32` (기본 3)
- `OrbFvgConfig::max_spread_ticks`

`etf_tick_size` 교정만 해도 122630 의 구조적 문제는 해결되지만, 모의투자 환경
114800 의 순간 3원 초과 문제는 임계치 조정 (5 tick 또는 그 이상) 으로 완화 가능.
단 실전에서는 넓힌 임계치가 불리할 수 있으므로 paper/real 별도 config 권장.

### 4. Degraded stuck 감시 이벤트 (선택)

`transition_to_degraded` 에서 "직전 Degraded 이후 경과 시간" 을 state 에 기록하고,
N 분 (예: 10분) 이상 연속 Degraded 면 `event_log.degraded_stuck` (severity=critical)
를 1 회 발행. operator 에게 알림.

### 5. 자매 이슈와의 결합 — OR 선계산

자매 문서 [2026-04-20-or-range-not-computed.md](./2026-04-20-or-range-not-computed.md)
§"권장 수정 방향 #1" 적용 시, tick size 버그를 고치더라도 **Degraded 전이가
OR 계산을 막지 않도록** 하는 안전망이 필요. 두 문서의 권장 조치는 **함께 적용**
되어야 완전 복구.

## 재발 방지 테스트

### Unit: etf_tick_size 교정 + 임계치 공식

```rust
#[test]
fn spread_ok_passes_when_single_tick_gap() {
    // KODEX 레버리지 범위에서 ask-bid = 50원 (1 tick) → pass.
    let (ask, bid) = (101_950, 101_900);
    let mid = (ask + bid) / 2;   // 101_925
    let tick = etf_tick_size(mid); // 수정 후: 50
    assert!(ask - bid <= tick * 3);
}

#[test]
fn spread_ok_fails_when_four_tick_gap_in_leverage() {
    // 50원 × 4 tick = 200원 gap → fail.
    let (ask, bid) = (102_000, 101_800);
    let mid = (ask + bid) / 2;
    let tick = etf_tick_size(mid);
    assert!(!(ask - bid <= tick * 3));
}
```

### Integration: 임계치 실측 기록

`preflight_gate_blocked` 이벤트 발행 시 metadata 에 `spread_raw`, `spread_tick`,
`spread_in_ticks` 가 채워지는지 assert.

### Regression: 122630 구조적 spread_exceeded 재현

stub quote `(ask=101_950, bid=101_900)` 주입 시 spread_ok=true 인지 assert.
수정 전 코드로는 fail → 수정 후 pass 가 되어야 한다.

## 관련 파일 · 참고

- `src/strategy/live_runner.rs:9762~9779` — `etf_tick_size` / `round_to_etf_tick`
- `src/strategy/live_runner.rs:4259~4340` — `refresh_preflight_metadata`
  (spread_ok 계산 지점)
- `src/strategy/live_runner.rs:823~851` — `PreflightMetadata` 구조체
- `src/strategy/live_runner.rs:4920~4958` — preflight_gate_blocked log_event
- `docs/monitoring/2026-04-18-execution-timing-implementation-plan.md` §"스프레드 필터"
  — 원안은 MarketableLimitWithBudget preflight gate 에서 사용. 현재 구현은 진입
  허용 판단에 직결되어 gate stuck 파급이 큼.
- 자매 문서: [2026-04-20-or-range-not-computed.md](./2026-04-20-or-range-not-computed.md)

## 다음 거래일 (4/21 화) 전 처리 여부

### 최소 필수 (이것 안 하면 4/21 또 동일 사고)

1. **`etf_tick_size` 50,000원 분기 추가** — 2줄 수정.
2. **자매 이슈의 OR 선계산** — 구조 재배열 (~20줄).

### 권장 (미루면 사후 진단 계속 불가)

3. **preflight metadata 에 실측치 4~5개 키 추가** — 파급 적음.

### 여유 되면 추가

4. 임계치 config 화.
5. degraded_stuck 감시 이벤트.
