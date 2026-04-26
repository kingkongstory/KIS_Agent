# 2026-04-24 P1-1 진입 timeout·부분체결 처리 보강 계획

## 배경

2026-04-24 장마감 분석에서 `entry_fill_timeout_ms=45,000` 설정으로도 `order_to_fill_ms=52,942` 체결을 기다리지 못해 timeout 이후 취소·부분체결 race가 다시 발생했다. 포지션 복구는 성공했지만 `actor_rule_mismatch`가 남았고, 다음 거래에서도 동일한 tail risk가 반복될 수 있다.

## 목표

- paper/passive 기본 진입 체결 대기 시간을 최소 60초로 상향한다.
- env `KIS_ENTRY_FILL_TIMEOUT_MS`로 운영자가 timeout을 조정할 수 있는 기존 경로를 유지한다.
- timeout 직후 부분체결 확인 경로가 포지션 수량 기준으로 안전하게 복구되는지 테스트한다.
- `cancel_fill_race_partial` 복구 전이를 actor shadow rule의 정상 경로로 인정해 운영 warning 노이즈를 제거한다.

## 비목표

- 진입 신호, OR/FVG/lock 계산식을 변경하지 않는다.
- 실전 marketable 주문 기본 timeout 3초 정책은 변경하지 않는다.
- 청산 `RestExecution` 실패 조사는 다음 P1-2 작업으로 분리한다.

## 구현 범위

- `src/config.rs`: paper/passive 기본 `KIS_ENTRY_FILL_TIMEOUT_MS` fallback을 60초로 상향하고 설정 테스트를 갱신한다.
- `src/strategy/live_runner.rs`: `LiveRunnerConfig::default()`와 clamp 범위, 단위 테스트를 60초 기준으로 갱신한다.
- `src/strategy/execution_actor.rs`: `flat -> entry_partial`의 `cancel_fill_race_partial` 복구 전이를 허용하는 pure rule과 테스트를 추가한다.
- `.env.example`: 운영 설정 예시를 60초 기준으로 갱신한다.

## 완료 결과

- paper/passive 기본 진입 체결 대기 시간을 60초로 상향했다.
- timeout 직후 REST 체결조회와 balance snapshot을 먼저 확인하고, 부분체결이 있으면 `EntryPending -> EntryPartial`로 복구한 뒤 잔량만 취소하도록 보강했다.
- 취소 직후 REST 조회가 기존에 확인한 부분체결 수량을 0으로 덮어쓰지 않도록 누적 수량 보존 로직을 추가했다.
- cancel/fill race로 뒤늦게 `Flat -> EntryPartial` 복구가 필요한 경우 actor shadow validation에서 정상 경로로 인정하도록 했다.
- 운영 `.env`에는 `KIS_ENTRY_FILL_TIMEOUT_MS` override가 없어 다음 재시작부터 코드 기본값 60초가 적용된다.

## 검증

- `cargo fmt -- --check`
- `cargo test entry_fill_timeout_default -- --nocapture`
- `cargo test strategy::execution_actor::tests::allowed_transition_covers_cancel_fill_race_partial_recovery`
- `cargo test config::tests`
- `cargo test`

전체 `cargo test` 결과: 440개 단위 테스트, 7개 main 테스트, doctest 2개 ignored 기준 통과. 기존 `strategy::parity::position_manager`의 unused assignment warning 2건은 남아 있으나 이번 변경과 무관하다.
