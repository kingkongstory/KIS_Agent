# 2026-04-24 review fix: session wait / quote gate

## 배경

코드 리뷰에서 두 가지 P1 회귀 가능성이 확인되었다.

- `wait_until` 이 목표 시각 경과 시 무조건 다음날로 넘겨 장중 재시작을 막을 수 있다.
- `quote_invalid` 상태가 `external_gates.spread_ok_override=Some(true)` 로 우회될 수 있다.

## 변경

- `wait_until_session_milestone` / `compute_session_wait_deadline` 으로 장중 milestone 대기를 세션 기준으로 계산한다.
- 장중에는 이미 지난 `or_start` / `or_5m_end` 를 즉시 통과하고, `force_exit` 이후에는 다음날 장 시작까지 대기한다.
- 야간 기동 후 다음날 09:00 까지 대기할 수 있으므로, DB 일일 거래 복구 기준일을 market wait 이후 다시 계산한다.
- `PreflightMetadata::all_ok()` 에 `quote_issue.is_none()` 을 포함해 external spread override 와 무관하게 비정상 호가를 최종 차단한다.

## 검증

- `cargo test wait_deadline -- --nocapture`
- `cargo test preflight_all_ok_requires_every_gate_true -- --nocapture`
- `cargo test`
- `cargo fmt -- --check` 실행 결과, 이번 변경 파일은 통과했지만 기존 미포맷 파일 2개가 남아 있다.
  - `src/main.rs`
  - `src/infrastructure/collector/kis_minute.rs`
