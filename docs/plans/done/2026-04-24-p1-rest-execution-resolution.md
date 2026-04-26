# 2026-04-24 P1-2 RestExecution 청산 체결가 확정 조사·보강 완료

## 배경

2026-04-24 장마감 분석에서 청산 거래가 `exit_resolution_source=balance_only`, `fill_price_unknown=true`로 저장됐다. 기존 코드에는 `ExitResolutionSource::RestExecution` 경로가 있으므로 신규 enum을 추가하지 않고, 기존 `WsNotice -> RestExecution -> BalanceOnly` fallback 체인에서 REST 확정이 실패한 원인을 먼저 추적했다.

## 조사 결과

- DB의 당일 `exit_final_fill` journal metadata는 `ws_received=false`, `fill_price_unknown=true`만 남아 있어 REST 조회 실패 원인을 판별할 수 없었다.
- 기존 `verify_exit_completion()`은 WS 수신 대기 후 REST 체결조회를 몇 차례 수행하고, 그 다음 balance 확인으로 넘어갔다.
- 시장가 청산 체결이 REST 조회 시점보다 늦게 반영되면 사전 REST 재시도를 모두 소진한 뒤 balance에서 `0` 보유만 확인되어 `BalanceOnly`로 확정될 수 있었다.
- 이 경우 balance가 flat을 확인한 직후 REST 체결조회를 한 번 더 수행해야 체결가 확정 가능성이 높아진다.

## 구현 내용

- REST 체결조회 결과를 `ExecutionQueryDiagnostics`로 구조화해 phase, attempt, 주문번호 매칭 여부, 총 row 수, 매칭 수량, 가격 산출 가능 수량, 평균가, 오류 사유를 기록한다.
- 체결조회 응답 파서를 분리해 여러 체결 row가 같은 주문번호로 나뉘어 들어오는 경우 수량 가중 평균가를 계산하도록 했다.
- `verify_exit_completion()`에 `post_balance_flat` REST 재조회 단계를 추가했다. balance가 `0` 보유를 확인했지만 체결가가 아직 없는 경우, `BalanceOnly`로 확정하기 전에 REST 체결조회를 한 번 더 시도한다.
- `exit_market`, `exit_final_fill` metadata에 REST 시도 횟수, 마지막 상태, 상세 diagnostics를 남기도록 했다.
- `RestExecution` enum은 기존 variant를 그대로 사용하고 중복 variant는 추가하지 않았다.

## 완료 기준

- `balance_only`로 내려가는 경우에도 다음 장마감 분석에서 REST 실패 원인을 metadata로 구분할 수 있다.
- balance flat 확인 직후 REST가 뒤늦게 체결내역을 제공하면 `resolution_source=rest_execution`으로 확정된다.
- 여러 체결 row의 평균가 계산과 실패 사유 분류가 단위 테스트로 검증된다.

## 검증

- `cargo fmt -- --check`
- `cargo test exit_verification_tests -- --nocapture`
- `cargo test`

## 비고

`cargo test`는 통과했다. 기존 `src/strategy/parity/position_manager.rs`의 unused assignment warning 2건은 이번 변경과 무관한 기존 경고다.
