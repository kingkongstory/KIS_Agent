# 2026-04-24 운영 레이어 DB 이력 검토

## 목적

DB에 남은 운영 이력으로 최근 수정이 실제 사고 원인을 충분히 막는지 확인하고, 아직 놓친 운영 리스크를 분리한다.

## 확인 범위

- `event_log` 의 장중/장외 이벤트 분포
- `trades`, `order_log`, `execution_journal` 의 주문·체결·상태 전이 이력
- `active_positions` 잔존 상태
- 2026-04-24 04:16:38 서버 재시작 이후 신규 오류 발생 여부

## 핵심 판단

최근 수정은 2026-04-23 사고 원인과 직접 연결되어 있으며 방향은 적절하다.

- `entry_fill_timeout_ms` 45초 상향은 타당하다. DB의 `trades.order_to_fill_ms` 기준 30초 초과 체결은 2026-04-16 36,250ms, 2026-04-17 36,309ms, 2026-04-23 37,398ms로 반복 관측됐다.
- post-flat reconcile 유예는 필요하다. 2026-04-23 `exit_pending → flat` 이후 약 3초 만에 `runner=83, KIS=0` mismatch가 발생해 `manual_intervention`으로 연결됐다.
- parser 교정과 quote sanity guard는 적절하다. 2026-04-23 `preflight_gate_blocked` 2,273건은 모두 `spread_exceeded`였고, metadata의 `spread_bid`가 누적거래량성 값으로 오염되어 있었다.
- `PreflightMetadata::all_ok()`의 `quote_issue.is_none()` fail-close 반영은 필요했다. 외부 spread override가 비정상 quote를 우회하지 못하게 막는다.
- session-aware wait 수정은 필요했다. 장중 재시작이 다음날 09:00으로 밀리는 것은 운영상 P1 장애이므로, 장중에는 지난 milestone을 즉시 통과해야 한다.

## DB 근거

### 수정 후 재시작 이후

조회 기준: `event_time >= '2026-04-24 04:16:38+09'`

- `api_error`: 0건
- `preflight_gate_blocked`: 0건
- `quote_invalid`: 0건
- `balance_reconcile_mismatch`: 0건
- `actor_rule_mismatch`: 0건
- `cancel_fill_race_detected`: 0건
- `ws_reconnect`: 17건
- `phase_change`: 2건

수정 후 시장 전 구간에서는 기존 P1 계열 이벤트가 재발하지 않았다. 단, 장외 WebSocket 재연결은 여전히 1~2분 간격으로 반복되고 있다.

### 2026-04-23 사고 구간

- `preflight_gate_blocked`: 2,273건
- `ws_reconnect`: 583건
- `api_error`: 65건
- `cancel_fill_race_detected`: 1건
- `actor_rule_mismatch`: 1건
- `balance_reconcile_mismatch`: 1건

체결 경로:

- 09:36:08 매수 주문 제출
- 09:36:38 `actor_timeout_expired`
- 09:36:39 취소 및 reconcile 시도
- 09:36:45 체결 완료 및 `cancel_fill_race_detected`
- 09:36:46 `flat → open` actor rule mismatch
- 09:37:23 청산 체결
- 09:37:26 `runner=83, KIS=0` reconcile mismatch

### 반복된 체결 지연

`order_to_fill_ms > 30000` 거래:

- 2026-04-16: 36,250ms
- 2026-04-17: 36,309ms
- 2026-04-23: 37,398ms

따라서 30초 timeout은 단발성이 아니라 paper/passive 주문 환경에서 반복적으로 부족했다.

## 남은 리스크

1. 장외 WS 재연결 폭주

2026-04-24 재시작 이후에도 `ws_reconnect`가 17건 발생했다. 장외 시간에는 실거래 가치가 낮고 rate limit 및 로그 노이즈만 만든다. 장외 backoff 확대 또는 장전 재연결 방식으로 바꿔야 한다.

2. 장외 balance/API polling

2026-04-24 00~04시 사이 `api_error` 39건이 발생했다. 재시작 이후 신규 API 오류는 없지만, 장외 polling이 다시 활성화되면 재발 가능성이 있다.

3. `balance_only` 청산 확정의 낮은 신뢰도

2026-04-23 거래는 `exit_resolution_source=balance_only`, `fill_price_unknown=true`로 기록됐다. 포지션 종료 자체는 맞지만 체결 가격의 신뢰도가 낮으므로 order query 또는 WS fill event 기반 보강이 필요하다.

4. race 메타데이터의 과거 데이터 한계

과거 `cancel_fill_race_detected` 이벤트에는 timing metadata가 없다. 신규 수정 이후부터는 충분한 진단 데이터가 쌓이지만, 45초가 충분한지는 다음 체결 이후 재평가해야 한다.

5. restart cleanup 로그 노이즈

`execution_journal`에 `orphan_restart_cleanup`이 16건 존재한다. holdings=0이면 안전하지만, 운영 관점에서는 중요 이벤트와 섞여 노이즈가 된다.

## 다음 수정 우선순위

1. 장외 WS/polling 스케줄링 수정
2. exit fill price resolution 보강
3. preflight reason dashboard 또는 운영 집계 추가
4. no-op orphan restart cleanup 로그 downsample 또는 severity 조정
5. 다음 체결 후 `cancel_fill_race_detected` 신규 metadata 검증

## 검증 명령

```powershell
psql -h localhost -p 5433 -U postgres -d kis_agent -c "\dt"
psql -h localhost -p 5433 -U postgres -d kis_agent -c "select event_type, severity, count(*) from event_log where event_time::date='2026-04-23' group by event_type,severity order by count desc;"
psql -h localhost -p 5433 -U postgres -d kis_agent -c "select event_type, severity, count(*) from event_log where event_time >= '2026-04-24 04:16:38+09' group by event_type,severity order by count desc;"
psql -h localhost -p 5433 -U postgres -d kis_agent -c "select id, stock_code, entry_time, order_to_fill_ms, fill_price_unknown, exit_resolution_source from trades where order_to_fill_ms is not null and order_to_fill_ms > 30000 order by entry_time;"
psql -h localhost -p 5433 -U postgres -d kis_agent -c "select id, event_time, stock_code, event_type, severity, message, metadata from event_log where event_type in ('balance_reconcile_mismatch','cancel_fill_race_detected','actor_rule_mismatch') order by event_time;"
```
