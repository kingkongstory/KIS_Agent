# 2026-04-23 spread gate / WS parser 원인 확인

## 목적

- 2026-04-22 거래 0건의 직접 원인이
  - 단순 무신호인지,
  - 아니면 preflight spread gate 입력값 오염인지
  다시 판정한다.
- 특히 실시간 체결/호가 파서가 `ask/bid` 필드를 잘못 읽고 있는지 확인한다.

## 확인 결과

## 1. 결론

- **사용자 가설이 맞다.**
- OR/FVG/lock 자체가 먼저 깨진 것이 아니라,
  **WS parser 상위 버그로 잘못 파싱된 `ask/bid`가 `latest_quote`에 들어가고,
  그 값으로 preflight spread gate가 구조적으로 차단**되고 있었다.
- 다만 2026-04-22는 replay 기준으로도 거래 0건이므로,
  "당일 실제 체결 기회를 gate가 놓쳤다"까지는 아직 증명되지 않았다.
  정확한 표현은:
  - **전략 경로는 정상**
  - **live 진입 gate 입력값은 비정상**
  - **당일은 결과적으로 replay도 0건**

## 2. 코드 근거

### 2-1. `latest_quote`는 gate 입력으로 직접 사용됨

- `src/strategy/live_runner.rs`
  - `spawn_price_listener` 에서 실시간 체결/호가 이벤트가 `latest_quote`를 갱신
  - `latest_quote_snapshot()` 이 preflight spread 평가 입력
  - `evaluate_spread_gate()` 가 `ask-bid <= tick * 3` 로 판정

즉 `latest_quote`가 틀리면 preflight gate가 잘못 동작한다.

### 2-2. 체결 파서 인덱스가 문서 스펙과 불일치

- 구현: `src/infrastructure/websocket/parser.rs`
  - `ask_price = parts[8]`
  - `bid_price = parts[9]`
  - `open = parts[11]`
  - `high = parts[12]`
  - `low = parts[13]`

- 문서: `docs/api/websocket.md`
  - `H0STCNT0`
    - `매도호가1 = 12`
    - `매수호가1 = 13`
    - `시가 = 21`
    - `고가 = 22`
    - `저가 = 23`

즉 현재 구현은 **`H0STCNT0` 인덱스를 공식 문서보다 앞쪽 필드로 읽고 있다.**

### 2-3. 호가 파서 인덱스도 문서 스펙과 불일치

- 구현: `src/infrastructure/websocket/parser.rs`
  - 매도호가 `index 1..10`
  - 매수호가 `index 11..20`
  - 매도잔량 `index 21..30`
  - 매수잔량 `index 31..40`

- 문서: `docs/api/websocket.md`
  - `H0STASP0`
    - 매도호가1~10 = `3..12`
    - 매수호가1~10 = `13..22`
    - 매도잔량1~10 = `23..32`
    - 매수잔량1~10 = `33..42`

즉 `H0STASP0` 파서도 **2칸씩 당겨 읽는 상태**다.

## 3. 운영 증거

2026-04-22 최초 `preflight_gate_blocked` 메타데이터:

- `122630`
  - `spread_ask = 108785`
  - `spread_bid = 107630`
- `114800`
  - `spread_ask = 1400`
  - `spread_bid = 1391`

같은 날짜 09:00 5분봉:

- `122630`
  - `high = 108785`
  - `low = 107630`
- `114800`
  - `high = 1400`
  - `low = 1391`

즉 spread gate에 들어간 `ask/bid`가 **실제 최우선호가라기보다 초기 캔들 high/low와 정확히 일치**했다.
이건 우연으로 보기 어렵고, parser 인덱스 오류 증거로 해석하는 것이 타당하다.

## 4. 왜 OR/FVG/lock은 정상으로 보나

- `candle_aggregator.rs` 의 분봉 집계는 `exec.price` 기반
- `live_runner.rs` 의 `latest_price`도 `exec.price` 기반
- 이전 replay 검증에서 2026-04-22는 legacy / parity / passive 모두 0건

즉:

- 캔들 생성과 OR 계산, FVG 탐지는 체결가 기반으로 돌아감
- 잘못 오염된 것은 `latest_quote` 기반 spread gate

## 5. 보조 확인

실행:

```powershell
cargo test signal_engine -- --nocapture
cargo test preflight_ -- --nocapture
```

결과:

- 신호 엔진 테스트 통과
- preflight 순수 로직 테스트 통과

추가 확인:

```powershell
cargo test test_parse_pipe_message -- --nocapture
```

- 기존 parser 테스트는 통과
- 다만 테스트 입력이 현재 구현 인덱스 가정에 맞춘 단순 샘플이라,
  **공식 문서 인덱스 불일치를 잡아주지 못한다**

즉 preflight 수학식 자체보다, **입력 스냅샷이 잘못 주입되는 문제**로 보는 것이 맞다.

## 6. 후속 조치 제안

1. `parser.rs` 의 `H0STCNT0`, `H0STASP0` 인덱스를 문서 기준으로 수정
2. parser 단위 테스트를 공식 인덱스 기준 샘플로 교체
3. `latest_quote`에 비정상 값이 들어오면
   - `ask < bid`
   - spread가 과도
   - ask/bid가 직전 OHLC high/low와 반복 일치
   같은 이상 징후를 event_log로 남기기
4. 수정 후 paper 장중 또는 recorded frame 기반 replay/smoke로
   `spread_in_ticks`가 실제 ETF 1호가 수준(예: 1틱 또는 소수 틱)으로 내려오는지 검증
