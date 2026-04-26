# 2026-04-23 H0STCNT0 parser 교정 결과

## 배경

- 2026-04-23 장 후 분석에서 `H0STCNT0` 파서가 잘못된 로컬 문서 `docs/api/websocket.md`를 기준으로 구현되어 있음을 확인했다.
- KIS 공식 GitHub 샘플 `legacy/Sample01/kis_domstk_ws.py`의 `contract_cols`와 대조한 결과, 우리가 사용하던 `ASKP1/BIDP1/CNTG_VOL` 인덱스가 잘못되어 있었다.

## 수정 내용

1. `src/infrastructure/websocket/parser.rs`
   - `H0STCNT0` 실행 필드 인덱스를 공식 샘플 기준으로 재교정
   - `OPEN/HIGH/LOW = 7/8/9`
   - `ASKP1/BIDP1/CNTG_VOL = 10/11/12`
   - `EXEC_MIN_FIELDS = 13`
   - parser 주석에 로컬 문서가 아니라 공식 샘플이 진실 원천임을 명시
2. 단위 테스트
   - 기존 잘못된 로컬 문서 기반 샘플을 제거
   - `test_parse_execution_uses_official_sample_indices`
   - `test_parse_execution_uses_askp1_bidp1_for_spread_fields`
   - 위 두 테스트로 `parts[10]/[11]/[12]` 회귀를 직접 고정
3. `docs/api/websocket.md`
   - `H0STCNT0` 필드 표를 공식 샘플 기준으로 전면 교정
   - `H0STASP0`도 공식 샘플 `bid_ask_cols` 기준임을 명시

## 검증

- `cargo test parser -- --nocapture`
- `cargo test preflight_ -- --nocapture`

모든 테스트가 통과했다. 별도 실패는 없었고, 기존 `position_manager.rs`의 `unused_assignments` 경고 2건만 재현되었다.

## 결론

- 2026-04-22~2026-04-23 parser 수정 계열 중 잘못된 방향이었던 `12/13/15/21/22/23` 기준을 폐기했다.
- 앞으로 `H0STCNT0` execution 인덱스의 기준은 로컬 문서가 아니라 공식 KIS 샘플과 회귀 테스트다.
