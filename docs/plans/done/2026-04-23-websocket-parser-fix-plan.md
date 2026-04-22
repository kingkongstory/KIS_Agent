# 2026-04-23 WebSocket parser 인덱스 수정 결과

## 배경

- 2026-04-22 라이브에서 `preflight_gate_blocked`가 연속 발생했고, 최초 차단 시점의 `ask/bid`가 실제 호가가 아니라 당일 5분봉 `high/low`와 일치했다.
- 원인은 `H0STCNT0`, `H0STASP0` parser가 `docs/api/websocket.md`의 공식 필드 인덱스와 다르게 구현되어 `latest_quote`를 오염시킨 데 있었다.

## 판단

- `ask/bid`만 부분 수정하면 비슷한 계열의 필드 어긋남이 남을 수 있으므로 `parse_execution`의 거래량, 시가, 고가, 저가까지 전부 다시 대조하는 것이 맞다.
- 특히 분봉 집계는 직전 체결량을 사용하므로, 누적거래량이 아니라 문서 기준 체결량 필드를 읽도록 함께 바로잡아야 한다.

## 작업 내용

1. `src/infrastructure/websocket/parser.rs`
   - `H0STCNT0` 필드 인덱스를 상수로 분리하고 공식 문서 기준으로 교정
   - `ask/bid/open/high/low`뿐 아니라 `stock_code/time/price/change/change_rate/volume`도 전부 상수 기반으로 재정의
   - `H0STASP0` 매도/매수 호가, 잔량, 총잔량 인덱스를 공식 문서 기준으로 교정
2. 단위 테스트
   - 문서 인덱스와 어긋난 기존 단순 샘플 대신, 의도적으로 더미 값을 섞은 샘플로 교체
   - `parse_execution`, `parse_orderbook`가 잘못된 인접 필드를 읽지 않는지 직접 검증 추가

## 검증

- `cargo test parser -- --nocapture`
- `cargo test preflight_ -- --nocapture`
- `cargo test signal_engine -- --nocapture`

모든 테스트가 통과했다. 별도 실패는 없었고, 기존 `position_manager.rs`의 `unused_assignments` 경고 2건만 재현되었다.

## 결론

- 이번 수정의 우선순위는 매우 높았고, 실제로 라이브 진입 차단의 상위 원인을 직접 제거하는 변경이었다.
- `parse_execution` 전체 필드 재검증도 적절한 판단이었다. `ask/bid`만 고쳤다면 `open/high/low`나 거래량 필드의 추가 오염 가능성을 남겼을 것이다.
