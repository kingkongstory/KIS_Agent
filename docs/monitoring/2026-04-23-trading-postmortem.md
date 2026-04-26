# 2026-04-23 자동매매 분석 — 첫 실거래 성공 속의 구조적 실패

## 요약 (TL;DR)

- **첫 실거래 체결** (122630 KODEX 레버리지, +0.245%) — 2026-04-22 parser 수정
  효과가 일부 입증됨.
- **그러나 구조적 이슈 3건 동시 관측**:
  1. 🔴 **Parser 필드 인덱스가 여전히 틀림** — KIS 공식 샘플(github
     `legacy/Sample01/kis_domstk_ws.py`) 대조 결과 **매도/매수호가1은 parts[10]/[11]**
     이어야 하는데 우리 구현은 parts[12]/[13]. docs/api/websocket.md 의 원천
     데이터가 잘못된 것이 2일 연속 파서 오류의 공통 원인.
  2. 🟡 **balance_reconcile_mismatch → manual_intervention 자동 진입** — 청산
     완료 3초 만의 reconcile 에서 runner state race 로 "숨은 포지션" 오판 →
     **첫 거래 후 122630 자동매매 정지**.
  3. 🟡 **cancel_fill_race_detected** — 30초 체결 대기 타임아웃과 체결통보
     도착이 동시에 발생. graceful 처리됐지만 재현 패턴 주의.
- **Preflight gate blocked 2,273건** (어제 8,970 → 75% 감소) — 전부 `spread_exceeded`.
  Parser 2차 버그가 해소 안 됐음을 증명하는 지표.
- **Replay 결과**: 3경로 모두 1건 체결 + 흑자 (0.32~0.54%). 라이브는 3분 15초
  빠른 진입 + 260~490원 비싼 진입가로 가장 작은 수익(0.245%).
- **심각도**: Critical. Parser 수정을 완전히 바로잡고 reconcile 유예 로직을 넣지
  않으면 내일(04-24) 또 동일 패턴 반복 예상.

## 관측된 증거

### 실거래 타임라인 (09:36~09:37 KST)

```
09:36:01.83  preflight_gate_blocked (spread_exceeded) — 여전히 활성
09:36:06.85  entry_signal [15m] Long entry=114170 SL=112655 TP=117957
09:36:06.85  flat → signal_armed
09:36:06.85  degraded → flat (degraded_cleared)  ← 우연의 순간
09:36:08.92  execute_entry_submitted (order 0000007865 @ 114150, 83주)
09:36:38.75  actor_timeout_expired (30초 경과) → entry_pending → flat
09:36:39.22  ReconcileBalance: holdings=83  ← 이미 체결됐는데 통보 지연
09:36:42.13  cancel_fallback (주문 취소 실패 → cancel_all_pending_orders)
09:36:45.41  🔴 cancel_fill_race_detected (gained=83, holding_after=83)
09:36:45.42  entry_executed: Long 83주 @ 114150 (슬리피지 -20원)
09:36:46.38  actor_rule_mismatch (flat → open actor rule 미설명)
09:36:46.38  flat → open (execute_entry_filled)
09:36:54.64  open → exit_pending (close_position_market)
09:37:23.81  exit_market: Long 83주 @ 114430 (TrailingStop, +0.245%)
09:37:23.81  exit_pending → flat
09:37:26.40  🔴 balance_reconcile_mismatch (runner_qty=83, kis_qty=0)
09:37:26.40  flat → manual_intervention (balance_reconcile_mismatch)
09:37:26.40  auto_close_skipped — 운영자 수동 처리 대기 (main_loop)
... 이후 122630 거래 차단, 114800 도 spread 게이트 진동만 반복
```

### Critical 이벤트 metadata

```
2026-04-23 09:36:45  cancel_fill_race_detected
{
  "gained": 83, "order_no": "0000007865",
  "avg_price": 114150, "limit_price": 114150,
  "holding_after": 83, "holding_before": 0,
  "fill_price_resolved": 114150
}

2026-04-23 09:37:26  balance_reconcile_mismatch
{
  "kis_qty": 0, "runner_qty": 83, "kis_qty_first": 0
}
```

### Preflight spread_exceeded 분포

어제 대비:

| 지표 | 04-22 (parts[8]/[9]) | 04-23 (parts[12]/[13]) | 변화 |
|---|---|---|---|
| preflight_gate_blocked | 8,970 | 2,273 | -75% |
| 진입 체결 | 0 | 1 (+0.245%) | 전환 |

종목별 / 시간대 분포 (오늘):

| 종목 | 시간 | 건수 |
|---|---|---|
| 122630 | 9시 | 310 (이후 manual_intervention 으로 poll 중단) |
| 114800 | 9시 | 484 |
| 114800 | 10시 | 479 |
| 114800 | 11시 | 227 |
| 114800 | 12시 | 334 |
| 114800 | 13시 | 186 |
| 114800 | 14시 | 185 |
| 114800 | 15시 | 68 |

→ **114800 은 종일 spread_exceeded 진동** (`flat ↔ degraded` 무한 반복).
진입 0건.

### Spread 실측치 — 절대 호가가 아님

```
2026-04-23 09:05 KST (122630, 실제가 107,710원대)
kst          | ask | bid       | spread    | tick | ticks
09:05:00.33  | 95  | 1,431,812 | -1,431,717| 50   | -28,634.34
09:05:05.34  | 18  | 1,437,750 | -1,437,732| 50   |  -28,754.64
09:05:10.35  | 52  | 1,448,328 | -1,448,276| 50   |  -28,965.52

2026-04-23 10:00 KST (114800, 실제가 1,400원대)
kst             | ask | bid         | spread       | ticks
10:00:01.99     | 2   | 158,510,061 | -158,510,059 | -3,170,201.18
10:00:07.01     | 1   | 158,522,896 | -158,522,895 | -3,170,457.90
10:00:17.03     | 1   | 158,534,552 | -158,534,551 | -3,170,691.02
```

- **bid 가 단조 증가** → 이건 **누적거래량(ACML_VOL)** 의 특성.
- ask 도 소액 정수 (체결거래량 CNTG_VOL 과 일치).
- 호가가 아니라 **거래량 필드를 호가로 잘못 읽고 있음**.

## 근본 원인 분석

### 🔴 P0 — Parser 필드 인덱스 2차 오류

KIS 공식 샘플 (github `koreainvestment/open-trading-api`,
`legacy/Sample01/kis_domstk_ws.py`) 의 `contract_cols` 리스트 기준:

| 인덱스 | 공식 필드 | 우리 parser.rs | 우리 docs/api/websocket.md |
|---|---|---|---|
| 0 | MKSC_SHRN_ISCD (종목코드) | 0 ✅ | 0 ✅ |
| 1 | STCK_CNTG_HOUR (체결시간) | 1 ✅ | 1 ✅ |
| 2 | STCK_PRPR (현재가) | 2 ✅ | 2 ✅ |
| 3 | PRDY_VRSS_SIGN (전일대비부호) | 3 ✅ | 3 ✅ |
| 4 | PRDY_VRSS (전일대비) | 4 ✅ | 4 ✅ |
| 5 | PRDY_CTRT (전일대비율) | 5 ✅ | 5 ✅ |
| 6 | WGHN_AVRG_STCK_PRC (가중평균가) | — | (미표기) |
| **7** | **STCK_OPRC (시가)** | 21 ❌ | 21 ❌ |
| **8** | **STCK_HGPR (고가)** | 22 ❌ | 22 ❌ |
| **9** | **STCK_LWPR (저가)** | 23 ❌ | 23 ❌ |
| **10** | **ASKP1 (매도호가1)** | 12 ❌ | 12 ❌ |
| **11** | **BIDP1 (매수호가1)** | 13 ❌ | 13 ❌ |
| **12** | **CNTG_VOL (체결거래량)** | 15 ❌ | 15 ❌ |
| **13** | **ACML_VOL (누적거래량)** | (미사용) | 9 ❌ |

우리 `docs/api/websocket.md` 의 표가 공식 스펙과 전혀 달라, 이 문서를 근거로
작성한 parser.rs 도 연쇄적으로 틀렸다. 2026-04-22 1차 수정도 이 잘못된 docs
에 기반했기 때문에 **다른 잘못된 답** (parts[8]/[9] → parts[12]/[13]) 으로
이동했을 뿐이다.

### 🔴 오늘 거래 1건이 성공한 이유 — 우연

event_log `09:36:06.853135  degraded → flat (degraded_cleared)` 가 핵심. 잘못된
parts[12]/[13] 값으로 계산한 spread 가 **우연히** tick×3 이하였던 한 순간이
있었고, 그 순간 entry_signal 이 발생 → 지정가 주문 체결.

재현성 없음. 내일 같은 조건 재현이 보장되지 않는다.

### 🟡 P1 — balance_reconcile_mismatch 자동 manual_intervention

`exit_pending → flat (flat_transition)` 기록은 09:37:23.81 에 있고, reconcile
스캔은 09:37:26.40 에 실행됐다. 3초 차이인데 reconcile 이 `runner_qty=83` 으로
봤다는 것은:

- runner 내부 `current_position` / 수량 상태 업데이트가 event_log transition
  기록보다 뒤늦게 flush 됨
- CLAUDE.md §"reconcile Pending/ManualHeld 가드" 는 Pending/ManualHeld 에만
  적용. **flat 전이 직후의 state 정리 race** 는 가드 대상 아님
- 결과적으로 청산 완료 → 3초 후 "runner_qty=83 vs kis_qty=0" → manual 승격

#### 파급 범위

- 이 전이는 event_log 기록만 발생. `active_positions` 에 row 가 없으면 (오늘
  처럼 flat 상태에서 manual 진입) DB 영속화가 안 되므로 **server restart 시
  자동 해제**.
- 반대로 재시작 없이 운영되면 오늘처럼 **첫 거래 후 해당 종목 자동매매 정지**
  (운영자 수동 clear 필요).

### 🟡 P1 — cancel_fill_race_detected

이건 실제 graceful 처리가 동작한 사례 (2026-04-14 사고 이후 안전장치가 만든
결과):

- 30초 지정가 대기 타임아웃 → cancel 경로 진입
- cancel 시도 순간 체결 통보 도착 (race)
- cancel 후 잔고 조회에서 83주 감지 → 정상 트레이드로 이어감

버그는 아니지만 **현재 관측 증거로는 race 윈도우 정보 부족**. metadata 에
`timeout_elapsed_ms`, `first_fill_notice_ms`, `cancel_attempted_ms` 기록
필요.

### 🟢 P2 — 라이브 vs 백테스트 진입 시각/가격 차이

Replay (`replay 122630 --date 2026-04-23 --rr 2.5 --stages 15m`):

| 경로 | 진입 시각 | 진입가 | 청산가 | 수익 | RR |
|---|---|---|---|---|---|
| legacy | 09:40:00 | 113,657 | 114,270 | +0.54% | 0.61 |
| parity/legacy | 09:40:00 | 113,662 | 114,270 | +0.53% | 0.60 |
| parity/passive | 09:40:00 | 113,890 | 114,259 | +0.32% | 0.30 |
| **라이브** | **09:36:45** | **114,150** | **114,430** | **+0.245%** | — |

라이브가 3분 15초 빨리, 260~490원 비싸게 진입 → 수익 절반 이하.

**이유**: 라이브는 WS 1분 틱 기반 FVG 탐지, 백테스트는 Yahoo 5분봉 close 기반.
라이브는 5분봉 마감을 기다리지 않고 즉시 진입.

구조적 차이. 문제라기보다 의도된 설계. 다만 Parser 수정 후 누적 표본을 보고
튜닝 여부 결정.

## 권장 수정

### 1. Parser 필드 인덱스 3차 수정 (즉시 필요, 한 줄급)

`src/infrastructure/websocket/parser.rs`:

```rust
const EXEC_STOCK_CODE_IDX: usize = 0;
const EXEC_TIME_IDX: usize = 1;
const EXEC_PRICE_IDX: usize = 2;
const EXEC_CHANGE_IDX: usize = 4;
const EXEC_CHANGE_RATE_IDX: usize = 5;
// 2026-04-23 수정: KIS 공식 H0STCNT0 스펙 기준 (legacy/Sample01/kis_domstk_ws.py)
const EXEC_OPEN_IDX: usize = 7;          // 기존 21 → 7
const EXEC_HIGH_IDX: usize = 8;          // 기존 22 → 8
const EXEC_LOW_IDX: usize = 9;           // 기존 23 → 9
const EXEC_ASK_PRICE_IDX: usize = 10;    // 기존 12 → 10 (실제 ASKP1)
const EXEC_BID_PRICE_IDX: usize = 11;    // 기존 13 → 11 (실제 BIDP1)
const EXEC_TRADE_VOLUME_IDX: usize = 12; // 기존 15 → 12 (실제 CNTG_VOL)
const EXEC_MIN_FIELDS: usize = 13;       // 기존 24 → 13
```

### 2. `docs/api/websocket.md` 전면 교정

`docs/api/websocket.md` 가 잘못된 원천이므로 반드시 정정. 이 문서를 근거로
다른 팀원/AI 가 다시 같은 오류를 낼 수 있다:

```markdown
### 실시간 체결가 (H0STCNT0) — KIS 공식 스펙

| 순서 | 필드명 | 설명 |
|------|--------|------|
| 0 | MKSC_SHRN_ISCD | 유가증권단축종목코드 |
| 1 | STCK_CNTG_HOUR | 주식체결시간 (HHMMSS) |
| 2 | STCK_PRPR | 주식현재가 |
| 3 | PRDY_VRSS_SIGN | 전일대비부호 |
| 4 | PRDY_VRSS | 전일대비 |
| 5 | PRDY_CTRT | 전일대비율 (%) |
| 6 | WGHN_AVRG_STCK_PRC | 가중평균주식가격 |
| 7 | STCK_OPRC | 시가 |
| 8 | STCK_HGPR | 고가 |
| 9 | STCK_LWPR | 저가 |
| 10 | ASKP1 | 매도호가1 |
| 11 | BIDP1 | 매수호가1 |
| 12 | CNTG_VOL | 체결거래량 (직전 체결 수량) |
| 13 | ACML_VOL | 누적거래량 |
| 14 | ACML_TR_PBMN | 누적거래대금 |
| ... | ... | ... |

**레퍼런스**: github.com/koreainvestment/open-trading-api
`legacy/Sample01/kis_domstk_ws.py` `contract_cols` 리스트.
```

### 3. 단위 테스트 pipe sample 재작성

`test_parse_execution_uses_documented_indices` 의 데이터 문자열과 assert 값을
새 인덱스 기준으로 재구성. 공식 스펙의 순서를 그대로 반영해 assert 하면 docs-
구현 불일치가 있을 때 즉시 fail.

```rust
#[test]
fn test_parse_execution_uses_documented_indices() {
    // KIS 공식 H0STCNT0 `legacy/Sample01/kis_domstk_ws.py` contract_cols 순서.
    // parts[0..=13] 최소 충족.
    let pipe = concat!(
        "0|H0STCNT0|1|",
        "005930^093015^72300^2^500^0.70^72250^",  // [0..=6]
        "72000^73000^71500^",                     // [7..=9] OPEN/HIGH/LOW
        "72500^72400^",                           // [10..=11] ASK1/BID1
        "321^",                                   // [12] CNTG_VOL
        "123456"                                  // [13] ACML_VOL (파싱 안 하지만 MIN_FIELDS 만족)
    );
    // assert 값 재작성:
    // - exec.open == 72000, exec.high == 73000, exec.low == 71500
    // - exec.ask_price == 72500, exec.bid_price == 72400
    // - exec.volume == 321
    ...
}
```

### 4. reconcile 유예 윈도우 (P1, 기다릴 수 없음)

`transition_execution_state(ExecutionState::Flat, ...)` 직후 N 초 (예: 10초)
동안 동일 종목의 balance_reconcile_mismatch → manual_intervention 승격을
**skip**. 기존 `should_skip_reconcile_for_lock()` helper 와 유사한 가드를
flat 전이에도 확장:

```rust
// src/strategy/live_runner.rs (의사 코드)
fn should_skip_reconcile_post_flat(&self) -> bool {
    let since_flat = self.state.last_flat_transition_at
        .map(|t| t.elapsed())
        .unwrap_or(Duration::from_secs(60));
    since_flat < Duration::from_secs(10)
}

async fn reconcile_once(&self) {
    if self.should_skip_reconcile_for_lock() { return; }
    if self.should_skip_reconcile_post_flat() {
        // event_log: reconcile_skipped_post_flat_window
        return;
    }
    ...
}
```

이 개선이 없으면 **내일도 첫 거래 후 자동매매 정지** 가능성.

### 5. cancel_fill_race_detected metadata 확장 (P1)

```rust
log_event("cancel_fill_race_detected", json!({
    "gained": gained,
    "holding_after": holding,
    "fill_price_resolved": fill_price,
    // 2026-04-23 추가: race 윈도우 진단용
    "timeout_elapsed_ms": elapsed_since_order_ms,
    "first_fill_notice_ms": first_notice_offset_ms,
    "cancel_attempted_ms": cancel_attempt_offset_ms,
    "entry_fill_timeout_ms_cfg": self.live_cfg.entry_fill_timeout_ms,
}))
```

race 빈도가 높으면 `entry_fill_timeout_ms: 30000` → `35000` 고려.

## 재발 방지 테스트

### Unit: parts[10]/[11] 이 실제로 호가여야 한다

```rust
#[test]
fn spread_detection_uses_askp1_bidp1() {
    let pipe = concat!(
        "0|H0STCNT0|1|",
        "122630^093015^108000^2^500^0.47^108010^",
        "108000^108100^107900^",   // OHLC
        "108050^107990^",          // ASK=108050, BID=107990 (1 tick spread)
        "321^1234567"
    );
    let msg = parse_ws_message(pipe).unwrap();
    if let WsMessage::Data(data) = msg {
        let rt = parse_execution(&data).unwrap();
        if let RealtimeData::Execution(exec) = rt {
            assert_eq!(exec.ask_price, 108050);
            assert_eq!(exec.bid_price, 107990);
            let spread = exec.ask_price - exec.bid_price;
            let tick = 50;  // 10만원 이상 구간
            assert!(spread <= tick * 3, "정상 호가는 통과해야");
        }
    }
}
```

### Integration: spread_ok metadata 실측치 sanity

```sql
-- DB 에 기록된 spread_ask/spread_bid 가 "현재가 ±5% 범위" 를 벗어나는 row
-- 가 있으면 parser 버그로 간주.
SELECT COUNT(*) FROM event_log
WHERE event_type = 'preflight_gate_blocked'
  AND DATE(event_time AT TIME ZONE 'Asia/Seoul') = CURRENT_DATE
  AND (
    (metadata->>'spread_ask')::bigint < 100
    OR (metadata->>'spread_bid')::bigint > 10000000
  );
-- 0 row 기대. 1+ 면 인덱스 회귀.
```

### Regression: reconcile 유예

Stub `exit_completed_at_now=true, reconcile_scan_at_now+2s` 로 호출해 manual
승격이 skip 되고, 11초 후 호출 시에는 승격됨을 assert.

## 관련 파일 · 참고

- `src/infrastructure/websocket/parser.rs:1~70` — EXEC_* 상수 / `parse_execution`
- `src/infrastructure/websocket/parser.rs:240~270` — 단위 테스트 sample
- `docs/api/websocket.md:56~80` — 잘못된 원천 문서 (교정 필요)
- `src/strategy/live_runner.rs` — `should_skip_reconcile_for_lock` 근처 (flat
  유예 추가 지점)
- `src/strategy/live_runner.rs` — `transition_to_degraded` / `clear_degraded_if_healthy`
- 어제 문서: [2026-04-22-parser-field-index.md](./)  *(아직 없음, 1차 수정 때는
  git commit 메시지로만 남음 — 이 문서가 2·3차 통합 postmortem)*
- 자매 문서: [2026-04-20-spread-exceeded-early-degraded.md](./2026-04-20-spread-exceeded-early-degraded.md)
  — 최초 spread_exceeded stuck 관측 + tick_size 버그
- 자매 문서: [2026-04-20-or-range-not-computed.md](./2026-04-20-or-range-not-computed.md)
- 공식 참고: `github.com/koreainvestment/open-trading-api/blob/main/legacy/Sample01/kis_domstk_ws.py`
  `contract_cols` 리스트

## 내일(2026-04-24) 장 전 체크리스트

### 최소 필수 (안 하면 또 0~1 거래)

1. **parser.rs `EXEC_OPEN_IDX=7`, `HIGH=8`, `LOW=9`, `ASK=10`, `BID=11`,
   `VOLUME=12`, `MIN_FIELDS=13` 로 수정** — 7줄 수정.
2. **단위 테스트 재작성** (`cargo test --lib parser::` 통과 필수).
3. **`docs/api/websocket.md` 실시간 체결가 표 교정** — 재발 방지 원천.
4. **`cargo build --release`**.
5. **서버 재시작** (`scripts/stop_timeout_smoke_server.ps1` →
   `scripts/start_timeout_smoke_server.ps1`).

### 권장 (첫 거래 후 자동매매 정지 방지)

6. **reconcile 유예 윈도우 10초** — `should_skip_reconcile_post_flat()` helper
   추가.
7. **cancel_fill_race metadata 확장** — race 윈도우 통계 수집 시작.

### 당일 09:05 KST 검증 SQL

```sql
SELECT stock_code,
       (metadata->>'spread_ask')::bigint AS ask,
       (metadata->>'spread_bid')::bigint AS bid,
       (metadata->>'spread_raw')::bigint AS spread,
       (metadata->>'spread_in_ticks')::float AS ticks,
       COUNT(*) AS cnt
FROM event_log
WHERE event_type = 'preflight_gate_blocked'
  AND DATE(event_time AT TIME ZONE 'Asia/Seoul') = '2026-04-24'
GROUP BY stock_code, ask, bid, spread, ticks
ORDER BY cnt DESC LIMIT 5;
```

**기대**:
- 122630 ask/bid 는 108,000원대 근처 (±몇백 원), spread 는 50~200원
- 114800 ask/bid 는 1,400원대 근처, spread 는 1~5원
- `preflight_gate_blocked` 총 건수 **수십~수백 수준** (어제 2,273건 → 1/10 이하)
- 두 종목 모두 진입 기회 다수 발생 예상
