# 2026-04-22 금일 데이터 전략 검증

## 목적

- 금일(2026-04-22) 데이터 추가 후 거래 0건이
  - 실제로 `돌파 + FVG` 진입 조건이 없어서인지,
  - 또는 구현/운영 오류 때문인지
  확인한다.

## 확인 항목

1. 금일 분봉/OR 데이터가 DB에 정상 적재됐는지 확인
2. 라이브 러너가 OR 계산과 preflight gate를 어떻게 처리했는지 확인
3. 같은 날짜를 `replay`로 재현해 전략 자체가 거래를 만들었는지 확인
4. 주문/체결 기록이 없는 이유가 무신호인지 오류인지 판정

## 실행 근거

### 1. 데이터 적재 / OR 계산

- `minute_ohlcv`
  - `122630`: 1분봉 380건, 5분봉 76건
  - `114800`: 1분봉 380건, 5분봉 77건
- `daily_or_range`
  - `122630`: 5m/15m/30m 모두 저장
  - `114800`: 5m/15m/30m 모두 저장

즉, **오늘 데이터 적재와 OR 계산은 정상 동작**했다.

### 2. preflight gate

- `event_log`
  - `122630`: `preflight_gate_blocked` 4,485건
  - `114800`: `preflight_gate_blocked` 4,485건
- 최초 차단 시점 메타데이터
  - `122630`: `spread_in_ticks=23.1`, threshold=`3`
  - `114800`: `spread_in_ticks=9.0`, threshold=`3`

즉, **신규 진입 경로는 장중 내내 `spread_exceeded`로 차단**됐다.

### 3. 전략 재현 결과

실행 명령:

```powershell
cargo run -- replay 122630 --date 2026-04-22 --stages 15m
cargo run -- replay 114800 --date 2026-04-22 --stages 15m
target\debug\kis_agent.exe replay 122630 --date 2026-04-22 --stages 5m,15m,30m
target\debug\kis_agent.exe replay 114800 --date 2026-04-22 --stages 5m,15m,30m
```

결과:

- `122630`: legacy / parity-legacy / parity-passive 모두 거래 0건
- `114800`: legacy / parity-legacy / parity-passive 모두 거래 0건

즉, **gate를 제거한 재현 경로에서도 오늘은 전략 거래가 생성되지 않았다.**

### 4. 캔들 조건 해석

- `122630`
  - 15분 OR 기준 `OR LOW` 하향 이탈 + bearish FVG 후보는 존재
  - 하지만 현재 신호 엔진은 `long_only=true` 설정에서 bearish 신호를 무시
- `114800`
  - 15분 OR 기준 상향 돌파 캔들은 있었음
  - 그러나 `A.high < C.low` 조건을 만족하는 bullish FVG가 없어 진입 불가

즉, **오늘 거래 0건의 직접 원인은 "롱 진입 가능한 돌파 + FVG 조합 부재"**다.

## 결론

- **오류 아님**: 오늘 데이터 적재, OR 계산, replay 재현은 모두 정상
- **운영상 차단은 있었음**: live는 `spread_exceeded`로 신규 진입이 계속 막힘
- **하지만 차단이 없어도 결과는 동일**: replay 상 모든 경로에서 거래 0건
- 따라서 오늘 0건은 **구현 오류로 신호를 놓친 사례가 아니라, 현재 전략 조건상 무신호 일자**로 판단한다.

## 추가 검증

실행 명령:

```powershell
cargo test signal_engine -- --nocapture
cargo test preflight_ -- --nocapture
```

결과:

- 신호 엔진 테스트 7건 통과
- preflight 관련 테스트 6건 통과

## 후속 메모

- live 운영 관점에서는 `spread_exceeded`가 하루 종일 유지된 점은 별도 운영 이슈다.
- 다만 오늘 거래 0건의 원인을 설명할 때는 **"gate 때문에 놓친 거래"가 아니라는 점**을 함께 명시해야 한다.
