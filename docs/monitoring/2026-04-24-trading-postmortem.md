# 2026-04-24 자동매매 장마감 분석

## 요약

- **거래 결과**: `114800` 1건 진입·청산, 2,000주, 진입가 1,385원, 청산가 1,385원, 손익률 0.00%.
- **잔고 상태**: 장마감 후 `active_positions=0`. `manual_intervention`, `balance_reconcile_mismatch`, `auto_close_skipped`는 발생하지 않았다.
- **전일 대비 개선 확인**: parser/preflight 계열의 `preflight_gate_blocked`, `quote_invalid`가 0건이며, 청산 직후 `reconcile_skipped_post_flat_window`가 동작해 false manual intervention을 막았다.
- **남은 P1 리스크**: `entry_fill_timeout_ms=45,000`도 부족했다. 오늘 실측 `order_to_fill_ms=52,942`로 timeout 이후 부분체결·취소 race가 다시 발생했다.
- **남은 P1 리스크**: 청산 체결가 확정은 여전히 `balance_only`, `fill_price_unknown=true`다. `WsNotice`와 `RestExecution` 경로가 청산 체결가 확정에 실패한 원인 조사가 필요하다.
- **장외 backoff 검증**: `KIS_OFF_HOURS_SUPPRESSION_ENABLED=true` 상태에서 16:00 이후 WS 재연결 delay가 2초에서 600초로 확대됐다. 15:45 이후 `ws_reconnect=8`, `ws_reconnect_deferred_off_hours=8`로 관측됐다.

## 데이터 기준

- 분석 일자: 2026-04-24
- DB 기준 이벤트 범위: `2026-04-24 00:00:01.228303+09` ~ `2026-04-24 17:22:05.430605+09`
- 총 event_log: 400건
- 서버 재시작 기준: 2026-04-24 08:51경 재시작 후 운영
- 장외 backoff flag: `.env` 기준 `KIS_OFF_HOURS_SUPPRESSION_ENABLED=true`

## 이벤트 집계

| event_type | severity | 건수 | 판단 |
|---|---:|---:|---|
| `ws_reconnect` | warn | 311 | 대부분 00~08시 장외 구간의 기존 과다 재연결 |
| `api_error` | warn | 35 | 장외·장중 잔고/현재가 조회 실패 및 rate limit |
| `api_error` | error | 9 | 00시대 HTTP 최종 실패 |
| `phase_change` | info | 18 | 초기화, OR 수집, 종료 전이 |
| `ws_reconnect_deferred_off_hours` | info | 8 | P0-1 장외 backoff 적용 확인 |
| `transition` | info | 7 | 114800 거래 상태 전이 |
| `actor_command_intent` | info | 4 | actor command 로그 정상 기록 |
| `actor_timeout_expired` | warning | 1 | 진입 체결 대기 45초 초과 |
| `cancel_fill_race_detected` | critical | 1 | timeout 후 부분체결 race 재발 |
| `actor_rule_mismatch` | warning | 1 | `flat -> entry_partial` 전이가 actor rule로 설명되지 않음 |
| `reconcile_skipped_post_flat_window` | info | 1 | flat 직후 reconcile 승격 유예 동작 |

발생하지 않은 이벤트:

| event_type | 건수 | 의미 |
|---|---:|---|
| `preflight_gate_blocked` | 0 | 04-23의 spread gate 오염 재발 없음 |
| `quote_invalid` | 0 | quote sanity가 오염 quote를 감지한 사례 없음 |
| `balance_reconcile_mismatch` | 0 | 청산 직후 false mismatch 재발 없음 |
| `manual_intervention` | 0 | 자동매매 정지 상태로 승격되지 않음 |
| `auto_close_skipped` | 0 | 수동개입 대기 때문에 자동청산이 막힌 사례 없음 |
| `ws_tick_gap` | 0 | 장중 tick gap 이벤트 없음 |

## 거래 타임라인

| 시각(KST) | 이벤트 | 내용 |
|---|---|---|
| 11:40:03.476 | `entry_signal` | `114800` 15m Long 신호. entry 1,385, SL 1,381, TP 1,395 |
| 11:40:17.371 | `entry_acknowledged` | 6,973주 지정가 매수 주문 ack. 주문번호 `0000021647` |
| 11:40:59.880 | `actor_timeout_expired` | entry fill timeout. 설정값 45초 기준 timeout |
| 11:41:02.392 | `actor_reconcile_balance` | timeout 직후 잔고 조회에서 2,000주 보유 확인 |
| 11:41:03.960 | `reconcile_skipped_post_flat_window` | flat 직후 settle window로 mismatch 승격 유예 |
| 11:41:07.190 | `cancel_fill_race_detected` | 취소 경로 중 2,000주 보유 증가 감지 |
| 11:41:07.205 | `entry_executed` | 2,000주 @ 1,385원으로 포지션 인정 |
| 11:41:08.440 | `exit_requested` | TrailingStop 조건으로 시장가 청산 요청 |
| 11:41:25.743 | `exit_market` | 2,000주 @ 1,385원 청산, 손익률 0.00% |
| 11:41:25.743 | `flat_transition` | `exit_pending -> flat`, 잔고 0 |

## 주문·체결 결과

| 구분 | 값 |
|---|---:|
| 주문 종목 | `114800` |
| 주문 수량 | 6,973주 |
| 최종 진입 수량 | 2,000주 |
| 진입 가격 | 1,385원 |
| 청산 가격 | 1,385원 |
| 손절/익절 | 1,386원 / 1,395원 |
| 청산 사유 | `TrailingStop` |
| `order_to_fill_ms` | 52,942ms |
| `exit_resolution_source` | `balance_only` |
| `fill_price_unknown` | true |

주문 로그 기준:

| 시각 | side | order_type | 수량 | 가격 | 상태 |
|---|---|---|---:|---:|---|
| 11:40:17.372 | Buy | 진입 | 6,973 | 1,385 | 발주 |
| 11:41:07.203 | Buy | 진입 | 2,000 | 1,385 | 체결 |
| 11:41:07.251 | Sell | TP지정가 | 2,000 | 1,395 | 발주 |
| 11:41:25.731 | Sell | 청산(TrailingStop) | 2,000 | 1,385 | 체결 |

## 수정사항 동작 평가

### 정상 동작 확인

1. **parser/preflight 방어선**
   - `preflight_gate_blocked=0`, `quote_invalid=0`.
   - 04-23의 오염 quote로 인한 spread gate 대량 차단은 재발하지 않았다.

2. **flat 직후 reconcile settle window**
   - `reconcile_skipped_post_flat_window`가 1회 기록됐다.
   - timeout 직후 runner 수량 0, KIS 잔고 2,000주가 순간적으로 엇갈렸지만 `balance_reconcile_mismatch`로 승격되지 않았다.
   - 04-23에 발생한 `flat -> manual_intervention` 자동 정지 패턴은 차단됐다.

3. **cancel/fill race metadata**
   - `timeout_elapsed_ms=45619`, `cancel_attempted_at_ms=48131`, `entry_fill_timeout_ms_cfg=45000`, `first_fill_notice_at_ms=null`이 기록됐다.
   - race 원인이 단순 추정이 아니라 timeout 설정값과 실제 지연의 충돌임을 판단할 수 있게 됐다.

4. **장외 WS backoff P0-1**
   - 15:45 이후 `ws_reconnect=8`, 전부 `off_hours_deferred=true`.
   - 재연결 delay는 `base_delay_ms=2000`에서 `effective_delay_ms=600000`으로 확대됐다.
   - 16:00~17:22 재연결 간격은 약 702~706초로 관측되어 70분 이상 장외 관찰 기준을 충족한다.

### 비정상 또는 미해결

1. **45초 진입 timeout 부족**
   - 오늘 `order_to_fill_ms=52,942`로 45초를 초과했다.
   - 04-23의 37.4초보다 더 느린 체결이며, 45초 상향은 충분하지 않았다.
   - 전체 `order_to_fill_ms>0` 10건 기준 p50은 6.382초, p90은 38.952초, max는 52.942초다. 표본은 작지만 tail risk가 timeout을 계속 초과하고 있다.

2. **부분체결 처리의 상태 전이 설명 부족**
   - `actor_rule_mismatch`: `flat -> entry_partial`, reason `cancel_fill_race_partial`.
   - 실제 포지션 복구는 성공했지만 actor shadow rule이 이 전이를 정상 경로로 인정하지 못한다.
   - 운영 관점에서는 false positive warning이지만, 장기적으로 state machine과 복구 경로가 불일치한다는 신호다.

3. **청산 체결가 확정 실패**
   - `exit_market` metadata: `ws_received=false`, `exit_resolution_source=balance_only`, `fill_price_unknown=true`.
   - `execution_journal.exit_final_fill`도 `resolution_source=balance_only`.
   - 체결가는 order_log와 balance fallback으로 기록됐지만, 체결통보 또는 REST 체결조회가 확정 근거로 남지 않았다.

4. **장외 API 오류 잔존**
   - 15:45 이후에도 `api_error=2`가 남았다. 둘 다 `/uapi/domestic-stock/v1/trading/inquire-balance` 1차 재시도 실패다.
   - WS backoff는 개선됐지만 balance polling 장외 억제는 아직 P0-2/P0-3 범위로 남아 있다.

## OR/FVG/lock 평가

### OR 생성

| 종목 | stage | high | low | source | 생성 시각 |
|---|---:|---:|---:|---|---|
| 114800 | 5m | 1,381 | 1,366 | candle | 09:05:00 |
| 114800 | 15m | 1,381 | 1,366 | candle | 09:15:02 |
| 114800 | 30m | 1,384 | 1,366 | candle | 09:30:00 |
| 122630 | 5m | 112,680 | 110,405 | candle | 09:05:00 |
| 122630 | 15m | 112,680 | 110,405 | candle | 09:15:02 |
| 122630 | 30m | 112,680 | 110,005 | candle | 09:30:00 |

분봉 저장 상태:

| 종목 | interval | 개수 | 첫 봉 | 마지막 봉 |
|---|---:|---:|---|---|
| 114800 | 1m | 380 | 09:00 | 15:19 |
| 114800 | 5m | 76 | 09:00 | 15:00 |
| 122630 | 1m | 380 | 09:00 | 15:19 |
| 122630 | 5m | 76 | 09:00 | 15:00 |

판단:

- OR 생성은 정상이다.
- `daily_or_range.source`는 `candle`로 기록됐다. 이전 상태 응답에서 보였던 `yahoo`와 달리 DB 기준 OR source는 candle이다.
- `114800`은 15m OR 기준 신호가 11:40에 발생했고 실제 주문까지 진행됐다.
- `122630`은 OR은 생성됐지만 오늘 진입 신호는 발생하지 않았다.

### FVG/lock

- `114800` 신호 metadata에서 `gap_top=1385`, `gap_bottom=1384`, `gap_size_pct=0.0007225`, `or_breakout_pct=0.0043447`가 기록됐다.
- daily lock 또는 position lock으로 인한 차단 이벤트는 없었다.
- 오늘은 "조건이 막힌 것"이 아니라 `114800`에서는 신호와 진입이 실제 발생했고, `122630`은 진입 신호가 없었던 날로 판단한다.

## 장외 backoff 검증

16:00 이후 WS 재연결은 아래처럼 약 11분 42초 간격으로 발생했다. 설정 delay 600초에 연결 시도·수신 대기 시간이 더해진 결과로 보인다.

| 시각 | 이전 이벤트와 간격 | delay_ms | off_hours_deferred |
|---|---:|---:|---|
| 16:00:01 | - | 600,000 | true |
| 16:11:44 | 702.5초 | 600,000 | true |
| 16:23:28 | 704.1초 | 600,000 | true |
| 16:35:13 | 705.7초 | 600,000 | true |
| 16:46:56 | 702.9초 | 600,000 | true |
| 16:58:41 | 704.6초 | 600,000 | true |
| 17:10:23 | 702.1초 | 600,000 | true |
| 17:22:05 | 702.0초 | 600,000 | true |

결론:

- P0-1은 실제 운영 DB에서 동작 확인됐다.
- `state_safe=true`, `blocked_by_state=false`로 기록되어 flat/무포지션 조건에서만 적용됐다.
- 다만 `api_error`는 15:45 이후 2건 남아 있어 balance polling 억제는 별도 과제로 유지한다.

## 다음 개선 우선순위

### P1-1. 진입 timeout 정책 재조정

권고:

- paper/passive 지정가 진입의 `entry_fill_timeout_ms`를 최소 60초로 상향하거나, timeout 전 `reconcile_balance`로 부분체결 여부를 먼저 확인한 뒤 취소 여부를 결정한다.
- 단순 timeout 상향만으로는 tail risk를 줄일 수 있지만, 부분체결 후 취소 race 자체는 남는다. 더 안전한 방향은 "timeout 직전 잔고/미체결 조회 → 부분체결이면 체결 수량 기준 포지션 전환 또는 잔량 취소" 순서다.

근거:

- 04-23: 37,398ms
- 04-24: 52,942ms
- 현재 설정: 45,000ms

### P1-2. RestExecution 실패 원인 조사

권고:

- `exit_final_fill`이 `balance_only`까지 내려간 이유를 코드와 로그로 추적한다.
- 기존 `ExitResolutionSource::RestExecution` 경로가 있다면 신규 enum 추가가 아니라 실패 조건, 재시도 간격, 응답 파싱, 주문번호 매칭 조건을 먼저 확인한다.
- 청산 직후 REST 체결조회 metadata에 `attempts`, `last_error`, `response_empty`, `order_no_matched`, `filled_qty_sum`을 남기는 방향을 검토한다.

### P1-3. actor rule에 cancel/fill partial 복구 경로 반영

권고:

- `cancel_fill_race_partial`에 의한 `flat -> entry_partial -> open`을 actor rule의 정상 복구 경로로 추가한다.
- 현재는 복구 자체는 성공하지만 `actor_rule_mismatch`가 warning으로 남아 운영자가 실제 규칙 위반과 복구 경로를 구분하기 어렵다.

### P2. 장외 polling 억제는 상태 가드 포함 후 진행

권고:

- 오늘 P0-1은 성공으로 보되, balance polling까지 억제하는 P0-2는 `ManualHeld`, `EntryPending`, `Open`, `ExitPending`, `manual_intervention_required`, active DB position을 모두 fail-close 조건으로 넣은 뒤 진행한다.
- 15:45 이후 API 오류가 2건으로 줄었지만 0은 아니다. 다만 hidden position 감지보다 rate limit 절감이 우선되어서는 안 된다.

## 최종 판단

오늘 수정분은 핵심 사고였던 parser 오염과 청산 직후 false manual intervention을 막는 데 효과가 있었다. 장마감 기준 잔고는 0이고 자동매매가 수동개입 상태로 멈추지 않았다.

그러나 체결 지연 tail risk가 더 커졌고, 45초 timeout은 오늘 바로 한계를 드러냈다. 다음 작업은 P0-2보다 **진입 timeout/부분체결 처리 보강**과 **RestExecution 실패 조사**를 우선해야 한다. P0-1은 70분 이상 장외 관찰에서 효과가 확인됐으므로 유지 가능하지만, balance polling 억제는 상태 가드 없이 확대하지 않는다.
