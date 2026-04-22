# Timeout Smoke 세션 메모

세션이 끝난 직후 아래 항목을 채운다. 빈 값은 `-` 로 남긴다.

## 메타

- **세션 날짜 (KST)**: YYYY-MM-DD
- **시나리오**: `armed-timeout` | `entry-fill-timeout` | `exit-fill-timeout`
- **종목**: `122630` | `114800`
- **환경**: `paper`
- **run_id**: (manifest 에 기록된 값과 동일)
- **서버 기동 방법**: `cargo run --release server` / `.\target\release\kis_agent.exe server`
- **서버 로그 경로**: `logs/server.log`

## 타임라인 (KST)

| 시각 | 상태 | 메모 |
|---|---|---|
| HH:MM:SS | (예: session start, OR 확정, FVG 감지, 지정가 발주, timeout, manual 전이, 수동 취소, session end) | |

## 장중 특이사항

- WS 단절/재연결 여부:
- DB write 실패 여부:
- 재시작 여부 (있다면 몇 시에, 왜):
- 수동 조작 여부 (HTS/MTS 에서 주문 취소, 포지션 청산 등):
- 예상 외 order 상태 (부분체결, 미접수, 거부):

## 시나리오별 체크

### armed-timeout (시나리오 A)
- [ ] `armed_watch_enter` 1건
- [ ] `armed_watch_exit` `detail='armed_entry_submit_timeout'`
- [ ] `armed_wait_enter` journal
- [ ] `armed_wait_exit` journal

### entry-fill-timeout (시나리오 B)
- [ ] `actor_timeout_expired` `phase='EntryFill'` 1건만
- [ ] `manual_intervention_required` 1건
- [ ] `active_positions.manual_intervention_required=true`
- [ ] `active_positions.execution_state='manual_intervention'`

### exit-fill-timeout (시나리오 C)
- [ ] `actor_timeout_expired` `phase='ExitFill'` 1건만
- [ ] `manual_intervention_required` 1건
- [ ] `actor_reconcile_balance`
- [ ] `active_positions.manual_intervention_required=true`
- [ ] `active_positions.execution_state='manual_intervention'`
- [ ] `active_positions.last_exit_order_no` 유지
- [ ] `strategy/status.manual_intervention_required=true`

## 판정 결과 (수집 직후 요약)

판정 세부는 별도 파일 `YYYY-MM-DD-timeout-smoke-result.md` 에 정리한다. 여기에는 한 줄 요약만.

- 5.1 `actor_timeout_expired` 단일성: `PASS` | `FAIL` | `NA`
- 5.2 exit manual 정합성: `PASS` | `FAIL` | `NA`
- 5.3 `armed_entry_submit_timeout` detail: `PASS` | `FAIL` | `NA`
