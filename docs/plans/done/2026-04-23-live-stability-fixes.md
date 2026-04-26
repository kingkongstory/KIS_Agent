# 2026-04-23 라이브 안정성 수정 계획

## 목적

- 2026-04-23 장중에 관측된 false timeout, reconcile race, quote 오염, race 진단 부족 문제를 동시에 줄인다.
- 전략 로직이 아니라 라이브 운용 경로의 안전장치를 강화한다.

## 작업 범위

1. `entry_fill_timeout_ms`
   - passive 기본값을 30초에서 45초로 상향
   - 관련 기본값/테스트/주석 동기화
2. reconcile settle window
   - `Flat` 전이 직후 10초 동안 reconcile manual 승격 유예
   - skip 이벤트를 별도 기록하고 helper/테스트 추가
3. quote sanity check
   - 비정상 ask/bid 를 `quote_invalid`로 분리 기록
   - 마지막 정상 quote 만 spread gate 에 사용
4. cancel_fill_race metadata
   - timeout/cancel/fill 타이밍 메타데이터 확장

## 완료 내용

1. `entry_fill_timeout_ms`
   - `AppConfig` passive 기본값을 `45_000ms`로 상향
   - `LiveRunnerConfig::default()` 및 관련 테스트/주석 동기화
   - 실전 `MarketableLimit` 기본값 `3_000ms`는 유지
2. reconcile settle window
   - `RunnerState.last_flat_transition_at` 추가
   - `execution_state -> Flat` 전이 시 시각 기록
   - `reconcile_post_flat_remaining_ms()` helper 추가
   - flat 직후 10초 안의 reconcile mismatch 는 `reconcile_skipped_post_flat_window`로 기록 후 승격 유예
3. quote sanity check
   - `RunnerState.last_quote_sanity_issue` 추가
   - execution/orderbook best quote 입력에 `validate_quote_sanity()` 적용
   - 비정상 quote 는 마지막 정상 quote 를 덮어쓰지 않고 `quote_invalid`로 기록
   - preflight 사유를 `spread_exceeded` 대신 `quote_invalid`로 분리
4. cancel/fill race metadata
   - `submit_ack_elapsed_ms`
   - `timeout_elapsed_ms`
   - `first_fill_notice_at_ms`
   - `cancel_attempted_at_ms`
   - `entry_fill_timeout_ms_cfg`

## 제외 범위

- 장 외 WS/polling 스케줄링(`#5`)은 별도 PR로 분리
- 전역 mismatch 다회 확인 승격은 genuine hidden position 탐지 지연 위험이 있어 이번 묶음에서는 제외

## 검증 결과

- `cargo test execution_state_tests -- --nocapture`
- `cargo test reconcile_ -- --nocapture`
- `cargo test parser -- --nocapture`
- `cargo test preflight_ -- --nocapture`
- `cargo test entry_fill_timeout -- --nocapture`

모든 테스트 통과. 기존 경고는 `src/strategy/parity/position_manager.rs`의 `unused_assignments` 2건만 유지.

## 후속 메모

- 장 종료 후 작업이라 실제 paper WebSocket smoke는 미실시
- 다음 장 시작 전 `quote_invalid` / `reconcile_skipped_post_flat_window` 이벤트가 기대대로 찍히는지 운영 로그 확인 필요
