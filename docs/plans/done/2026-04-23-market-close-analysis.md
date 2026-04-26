# 2026-04-23 장 마감 결과 분석

## 결과 요약

- `trades` 기준 실제 체결은 `122630` 1건, `114800` 0건이었다.
- `122630` 실거래 결과는 `09:36:45` 진입, `09:37:23` 청산, `+0.2453%`였다.
- `daily_report`는 전체 1승으로 기록됐지만, 시스템 이벤트 수치는 종목별로 전역 이벤트를 중복 집계하므로 원본 `event_log`를 같이 봐야 한다.
- 분봉/OR 데이터 적재는 정상이다.
  - `122630`: 1분봉 381개, 5분봉 76개, `15m OR=113600/111325`
  - `114800`: 1분봉 381개, 5분봉 77개, `15m OR=1374/1359`

## 핵심 문제

### 1. `114800` 미진입은 전략 무신호가 아니라 live preflight 차단이다

- `event_log`에서 `114800`은 `preflight_gate_blocked=1963`, `entry_signal=0`이었다.
- 같은 날짜 replay는 `114800`에 대해 세 경로 모두 1건 거래를 재현했다.
  - `12:05~12:10`, 약 `+1.08%`
- 즉, 오늘 `114800`의 미진입은 전략 조건 부재가 아니라 live 운용 경로가 신호 탐색 전에 막힌 결과다.

### 2. 금일 live에도 교정 전 parser 증상이 남아 있었다

- `114800`의 12:05 구간 `preflight_gate_blocked` 메타데이터는 `spread_ask=20000`, `spread_bid=294564483` 같은 비정상 값을 기록했다.
- `122630`도 장중 대부분 `spread_ask` 수십 원, `spread_bid` 수백만 원 수준으로 기록됐다.
- 이는 정상 호가가 아니며, `latest_quote` 입력이 오염된 상태로 `spread gate`에 들어갔다는 뜻이다.
- 오늘 장 로그 기준으로는 parser 인덱스 교정이 런타임에는 아직 반영되지 않았다고 보는 것이 타당하다.

### 3. 유일한 체결 1건도 timeout → cancel → fill race → manual 전이로 이어졌다

- `execution_journal` 기준 `122630` 진입 주문은 `09:36:08` 제출, `09:36:46` 최종 체결로 `order_to_fill_ms=37398`이었다.
- 현재 passive limit 기본 timeout은 `30_000ms`라서, 체결 직전 `actor_timeout_expired`와 `cancel_fallback`이 먼저 발생했다.
- 이후 `cancel_fill_race_detected`, `actor_rule_mismatch`, `balance_reconcile_mismatch`가 연쇄 발생했고, 시스템은 `manual_intervention`으로 전이됐다.
- 즉, 수익 거래였지만 운용 상태 기계는 이를 깔끔하게 종결하지 못했다.

### 4. 장 외 시간 연결/조회 폭주가 지속됐다

- `ws_reconnect`는 `00:00:01`부터 `17:00:00`까지 총 341건 발생했다.
- 00~04시는 시간당 35건 수준으로 반복되었고, 16시 이후에도 다시 시간당 35건 수준으로 증가했다.
- `api_error`는 총 20건이며, 19건이 `/uapi/domestic-stock/v1/trading/inquire-balance` 호출 제한 초과였다.
- 이는 장 외 시간에도 WS/잔고 조회 루프가 계속 돌고 있어 시스템 노이즈와 rate limit을 스스로 유발한다는 뜻이다.

## 개선 방향

### 우선순위 1. parser 교정본을 장 시작 전 반드시 배포하고, quote sanity check를 추가한다

- 이미 수정한 `src/infrastructure/websocket/parser.rs` 인덱스 교정본을 실제 운용 프로세스에 반영해야 한다.
- 추가로 preflight 입력 직전에 sanity check가 필요하다.
  - `ask <= 0`, `bid <= 0`, `ask < bid`, `mid`가 현재가 대비 비정상적으로 큰 경우
  - 이 경우 `spread_exceeded`로 막지 말고 `quote_invalid`로 분리 기록 후 마지막 정상 호가를 유지하거나 재수신 대기
- 그래야 parser/피드 이상이 전략 미진입으로 조용히 변질되지 않는다.

### 우선순위 2. passive limit entry timeout 정책을 재설계한다

- 오늘 실제 fill은 37.4초였는데 기본 timeout은 30초라 false timeout이 발생했다.
- 개선안:
  - `PassiveZoneEdge`의 기본 `KIS_ENTRY_FILL_TIMEOUT_MS`를 45~60초로 상향
  - timeout 직후 곧바로 `flat`으로 내리지 말고 `reconcile_pending` 같은 중간 상태를 둔다
  - cancel 전 마지막 주문 상태/보유 수량을 한 번 더 확인해 fill race를 줄인다

### 우선순위 3. exit 이후 reconcile/manual 전이를 더 보수적으로 만든다

- 오늘은 이미 `exit_market`로 `holdings_after=0`가 확인됐는데 직후 `balance_reconcile_mismatch`로 manual에 들어갔다.
- 개선안:
  - `exit_final_fill` 이후 짧은 settle window 동안은 mismatch 판단을 지연
  - `balance_only`로 청산 확정된 경우 runner 상태와 잔고 API 사이의 짧은 전파 지연을 허용
  - `manual_intervention` 진입 전 동일 mismatch를 2~3회 연속 확인하도록 강화

### 우선순위 4. 장 외 시간 WS/잔고 조회를 중지한다

- auto-start를 유지하더라도 실 구독과 잔고 polling은 장 시작 직전~장 종료 직후로 제한해야 한다.
- 장 외 시간에는
  - WS 연결 자체를 열지 않거나
  - reconnect backoff를 대폭 늘리고
  - `inquire-balance` 주기를 줄이거나 비활성화해야 한다.
- 그래야 `ws_reconnect`, `api_error` 노이즈를 줄이고 실제 장중 오류만 보이게 된다.

## 결론

- 오늘 손익 자체는 `+0.2453%` 1승으로 마감했지만, 운용 품질 관점에서는 정상 운영으로 보기 어렵다.
- `114800`의 기대 거래 1건을 parser/spread gate 문제로 놓쳤고, `122630` 실거래 1건도 상태 기계가 timeout/fill race를 매끄럽게 처리하지 못했다.
- 내일 장 전 필수 조치는 `parser 재배포 + quote sanity check + passive timeout 조정`이다.
