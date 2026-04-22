# 2026-04-21 Timeout Smoke 데이터 수집 계획

## 목적

`#5.d-wire` 이후 장중 paper smoke 에서 아래 3가지를 **증거 기반**으로 확인하기 위한
수집 계획이다.

1. `actor_timeout_expired` 가 실제 timeout 상황에서 **정확히 1회** 기록되는가
2. `TimeoutExpired { ExitFill }` 이후 `manual_intervention_required` 이벤트, 러너 메모리 상태,
   DB `active_positions.manual_intervention_required` / `execution_state='manual_intervention'`
   가 서로 일치하는가
3. `SignalArmed` timeout 상황에서 `execution_journal.phase='armed_wait_exit'` +
   `metadata.detail='armed_entry_submit_timeout'` 가 남는가

이 문서는 "어떤 데이터를 모아야 위 3개를 판정할 수 있는가" 와 "그 데이터를 어디에 저장할 것인가"
를 정리한다.

## 배경

2026-04-21 야간 smoke 에서는 서버 기동, DB 연결, health API, auto-start 기본 경로는 확인했지만
장 종료 후라 timeout/manual/armed 실경로를 밟지 못했다. 따라서 다음 장중 smoke 는 단순 서버
기동 확인이 아니라 **시나리오별 증거 수집 세션** 으로 운영해야 한다.

현재 코드 기준 source of truth 는 다음과 같다.

- `event_log`: 운영 이벤트 타임라인
- `execution_journal`: 주문/armed/restart phase 타임라인
- `active_positions`: 수동 개입 플래그와 실행 상태 영속화
- `/api/v1/monitoring/armed-stats`: 러너 메모리의 armed stats / 현재 armed signal / manual/degraded 상태
- `/api/v1/strategy/status`: UI/운영 관찰용 상태 스냅샷
- 서버 stdout/stderr 로그: DB write 실패, actor mismatch, HTTP/WS 오류 보조 증거

## 1. 수집 대상 데이터 목록

### 1.1 공통 런 메타데이터

모든 시나리오에서 아래 메타를 함께 남겨야 한다.

| 항목 | 의미 | 수집 원천 | 저장 위치 |
|---|---|---|---|
| `session_date_kst` | 세션 날짜 | 수동 기록 | `manifest.json` |
| `scenario_id` | 시나리오 식별자 (`armed-timeout`, `entry-fill-timeout`, `exit-fill-timeout`) | 수동 기록 | `manifest.json` |
| `environment` | `paper` / `real` | `.env`, 런타임 설정 | `manifest.json` |
| `stock_code` | 대상 종목 (`122630`, `114800`) | 수동 기록 | `manifest.json` |
| `run_started_at_kst` | 수집 시작 시각 | 수동 기록 | `manifest.json` |
| `run_ended_at_kst` | 수집 종료 시각 | 수동 기록 | `manifest.json` |
| `server_log_file` | 해당 세션 서버 로그 경로 | 수동 기록 | `manifest.json` |
| `notes` | 장중 특이사항, 재시작 여부, 수동 조작 여부 | 수동 기록 | `notes.md` |

### 1.2 `event_log` 에서 반드시 수집할 데이터

`event_log` 는 timeout 발생, manual 진입, armed enter/exit, actor 보조 이벤트를 시간순으로
재구성하는 1차 증거다.

| event_type | 확인 목적 | 필수 컬럼/메타 |
|---|---|---|
| `actor_timeout_expired` | timeout 1회 기록 여부 | `event_time`, `stock_code`, `severity`, `message`, `metadata.phase`, `metadata.state`, `metadata.reason` |
| `manual_intervention_required` | manual 진입 기록 여부 | `event_time`, `stock_code`, `severity`, `message`, `metadata` 전체 |
| `armed_watch_enter` | armed 감시 시작 | `event_time`, `metadata.signal_id`, `metadata.budget_ms`, `metadata.signal_entry_price`, `metadata.current_at_signal` |
| `armed_watch_exit` | armed 종료 결과 | `event_time`, `metadata.outcome`, `metadata.detail`, `metadata.duration_ms`, `metadata.signal_id` |
| `actor_command_intent` | timeout 이후 actor 가 어떤 command 를 냈는지 보조 확인 | `event_time`, `metadata.command`, `metadata.event`, `metadata.reason` |
| `actor_invalid_transition` | timeout 처리 중 잘못된 전이 여부 | `event_time`, `metadata.from`, `metadata.event`, `metadata.reason` |
| `actor_rule_mismatch` | shadow validation mismatch 여부 | `event_time`, `metadata.from`, `metadata.to`, `metadata.reason` |
| `actor_reconcile_balance` | 청산 후 holdings 재확인 흐름 | `event_time`, `metadata.source_event`, `metadata.reason`, `metadata.holdings_qty`, `metadata.avg_price`, `metadata.error` |

### 1.3 `execution_journal` 에서 반드시 수집할 데이터

`execution_journal` 은 armed phase 와 주문 phase 를 분리해서 보여주므로, event_log 단독으로는
보이지 않는 "phase 정합성" 을 확인하는 2차 증거다.

| phase | 확인 목적 | 필수 컬럼/메타 |
|---|---|---|
| `armed_wait_enter` | armed watcher 시작 | `created_at`, `stock_code`, `execution_id`, `signal_id`, `phase`, `intended_price`, `sent_at`, `metadata.budget_ms`, `metadata.tick_interval_ms`, `metadata.signal_entry_price`, `metadata.current_at_signal` |
| `armed_wait_exit` | armed timeout detail 확인 | `created_at`, `stock_code`, `execution_id`, `signal_id`, `phase`, `resolution_source`, `reason`, `final_fill_at`, `metadata.outcome`, `metadata.detail`, `metadata.severity`, `metadata.duration_ms` |
| `entry_submitted` / `entry_acknowledged` / `entry_partial` / `entry_final_fill` | entry timeout 직전/직후 주문 상태 | `created_at`, `execution_id`, `signal_id`, `broker_order_id`, `phase`, `sent_at`, `ack_at`, `first_fill_at`, `final_fill_at`, `filled_qty`, `holdings_before`, `holdings_after`, `reason`, `metadata` |
| `exit_final_fill` / `exit_retry_final_fill` | exit timeout 전후 체결 여부 | `created_at`, `execution_id`, `broker_order_id`, `phase`, `final_fill_at`, `filled_qty`, `holdings_before`, `holdings_after`, `resolution_source`, `reason`, `metadata` |
| `exit_submit_failed` / `exit_submit_rejected` / `exit_retry_still_remaining_manual` | exit fill timeout 과 구분 | `created_at`, `phase`, `reason`, `error_message`, `metadata` |

### 1.4 `active_positions` 에서 반드시 수집할 데이터

`active_positions` 는 timeout 이후 DB 영속 상태가 맞는지 확인하는 핵심 증거다.

| 컬럼 | 확인 목적 |
|---|---|
| `stock_code` | 대상 종목 식별 |
| `manual_intervention_required` | timeout 후 manual 플래그 일치 여부 |
| `execution_state` | `manual_intervention` / `exit_pending` / `entry_pending` 등 최종 상태 확인 |
| `last_exit_order_no` | ExitFill timeout 상관관계 |
| `last_exit_reason` | 어떤 exit reason 에서 timeout 이 났는지 |
| `pending_entry_order_no` | Entry/armed timeout 구간 상관관계 |
| `pending_entry_krx_orgno` | 진입 pending 주문 조회용 보조 증거 |
| `signal_id` | journal / event_log 와 상관관계 |
| `quantity` | timeout 시점 DB 보유 수량 |
| `updated_at` | state 갱신 시각 확인 |

### 1.5 API 스냅샷으로 수집할 데이터

장중에는 DB와 함께 메모리 상태를 캡처해야 "이벤트는 찍혔는데 러너 메모리가 안 맞는" 상황을 잡을 수 있다.

| API | 필수 필드 | 용도 |
|---|---|---|
| `GET /api/v1/monitoring/armed-stats` | `stock_code`, `current_armed_signal_id`, `manual_intervention_required`, `degraded`, `execution_state`, `stats.*`, `ready_avg_duration_ms`, `aborted_avg_duration_ms` | armed watcher 실시간 상태와 종료 후 통계 확인 |
| `GET /api/v1/strategy/status` | 종목별 `active`, `state`, `message`, `manual_intervention_required`, `degraded_reason` | 운영 화면 상태와 DB/event 정합성 확인 |
| `GET /api/v1/monitoring/health` | `db_connected`, `active_runners`, `ws.notification_ready`, `ws_tick_stale` | smoke 세션 유효성 확인 |

### 1.6 서버 로그에서 수집할 데이터

| 로그 종류 | 확인 목적 |
|---|---|
| 해당 세션 `cargo run` stdout/stderr | DB write 실패, HTTP/WS 오류, panic, timeout 이유 문자열 보조 확인 |
| 수집 스크립트 실행 로그 | 쿼리/HTTP export 자체 실패 여부 확인 |

## 2. 시나리오별 최소 수집 세트

### 2.1 시나리오 A — `SignalArmed` timeout

목표: `armed_wait_exit(detail='armed_entry_submit_timeout')` 가 남는지 확인.

필수 증거:

1. `event_log.armed_watch_enter` 1건
2. `event_log.armed_watch_exit` 1건
   - `metadata.outcome='aborted'` 또는 설계상 해당 outcome
   - `metadata.detail='armed_entry_submit_timeout'`
3. `execution_journal.armed_wait_enter` 1건
4. `execution_journal.armed_wait_exit` 1건
   - `metadata.detail='armed_entry_submit_timeout'`
   - `resolution_source` 와 `metadata.outcome` 일치
5. 같은 시점 전후 `armed-stats` API 스냅샷 2회 이상
   - 시작 직후 1회
   - 종료 직후 1회

### 2.2 시나리오 B — `EntryFill` timeout

목표: `actor_timeout_expired` 가 1회만 찍히고, entry timeout 이후 manual 전이가 한 번만 발생하는지 확인.

필수 증거:

1. `event_log.actor_timeout_expired`
   - `metadata.phase='EntryFill'`
   - 동일 `stock_code` / 근접 시간대에 중복 2건 이상 없어야 함
2. `event_log.manual_intervention_required`
3. `execution_journal` 의 entry phase 묶음
   - 최소 `entry_submitted`
   - 가능하면 `entry_acknowledged`
   - timeout 직전 최종 phase 확인
4. timeout 직후 `active_positions`
   - `manual_intervention_required=true`
   - `execution_state='manual_intervention'`
5. `strategy/status`, `armed-stats` 스냅샷

### 2.3 시나리오 C — `ExitFill` timeout

목표: `TimeoutExpired { ExitFill }` 이후 manual 이벤트, DB manual 플래그, 실행 상태가 모두 일치하는지 확인.

필수 증거:

1. `event_log.actor_timeout_expired`
   - `metadata.phase='ExitFill'`
2. `event_log.manual_intervention_required`
3. `event_log.actor_reconcile_balance`
   - timeout 전후 holdings 조회 흐름 확인
4. `execution_journal`
   - 직전 exit 관련 phase
   - 체결/재시도/잔량 여부를 보여주는 row
5. timeout 직후 `active_positions`
   - `manual_intervention_required=true`
   - `execution_state='manual_intervention'`
   - `last_exit_order_no` 유지
6. `strategy/status` 스냅샷
   - UI/운영 상태도 `수동 개입 필요` 로 맞는지 확인

## 3. 저장 계획

## 3.1 저장 위치

세션별 증거는 아래 경로에 저장한다.

```text
docs/monitoring/evidence/YYYY-MM-DD-timeout-smoke/
  manifest.json
  notes.md
  event-log.csv
  execution-journal.csv
  active-positions-before.json
  active-positions-after.json
  armed-stats-before.json
  armed-stats-after.json
  strategy-status-before.json
  strategy-status-after.json
  health.json
  server.log
```

원칙:

- DB 원본 테이블은 그대로 두고, 위 경로에는 **세션 시점 스냅샷** 만 저장한다.
- `server.log` 는 원본 경로를 복사하거나 심볼릭 링크 대신 파일 복사본으로 보존한다.
- 동일 날짜에 여러 번 수행하면 하위 폴더를 추가로 나눈다.
  예: `docs/monitoring/evidence/2026-04-22-timeout-smoke/run-01/`

## 3.2 파일 형식

| 파일 | 형식 | 이유 |
|---|---|---|
| `manifest.json` | JSON | 자동화/재실행 친화적 |
| `notes.md` | Markdown | 운영자 메모 |
| `event-log.csv` | CSV | 필터/집계 용이 |
| `execution-journal.csv` | CSV | phase 타임라인 비교 용이 |
| `active-positions-*.json` | JSON | nullable 필드 보존 |
| `armed-stats-*.json` | JSON | 중첩 stats 구조 보존 |
| `strategy-status-*.json` | JSON | UI 상태 원형 보존 |
| `health.json` | JSON | smoke 세션 유효성 증명 |
| `server.log` | TEXT | tracing/log raw 증거 |

## 4. 수집 절차

### 4.1 세션 시작 직후

1. DB / 서버 / WS health 확인
2. `health.json` 저장
3. `strategy-status-before.json`, `armed-stats-before.json`, `active-positions-before.json` 저장
4. `manifest.json` 에 세션 메타 기록

### 4.2 시나리오 발생 중

1. 대상 종목과 시각을 `notes.md` 에 기록
2. 해당 시나리오가 끝난 즉시 아래를 저장
   - `event-log.csv` (최근 N분)
   - `execution-journal.csv` (최근 N분)
   - `strategy-status-after.json`
   - `armed-stats-after.json`
   - `active-positions-after.json`
3. 서버 로그를 해당 시각 기준으로 보존

### 4.3 세션 종료 후

1. `manifest.json` 에 종료 시각 및 결과 기입
2. 증거 폴더 누락 파일 점검
3. 필요한 경우 별도 분석 문서
   - `docs/monitoring/YYYY-MM-DD-timeout-smoke-result.md`
   를 작성해 판정 결과를 남긴다

## 5. 판정 기준

### 5.1 `actor_timeout_expired`

합격:

- 동일 사고 창에서 `event_log.actor_timeout_expired` 가 1건
- `metadata.phase` 가 기대값과 일치
- 이후 actor invalid transition / rule mismatch 가 없음

실패:

- 동일 창에서 2건 이상 중복 기록
- timeout 은 났는데 event_log row 가 없음
- timeout 이후 invalid transition / rule mismatch 동반

### 5.2 `ExitFill timeout -> manual 정합성`

합격:

- `actor_timeout_expired.phase='ExitFill'`
- `manual_intervention_required` event 가 뒤이어 기록
- `active_positions.manual_intervention_required=true`
- `active_positions.execution_state='manual_intervention'`
- `strategy/status.manual_intervention_required=true`

실패:

- event 는 있는데 DB manual 플래그가 false
- DB 는 manual 인데 status/event 가 비어 있음
- `execution_state` 가 `exit_pending` 등으로 남아 있음

### 5.3 `armed_entry_submit_timeout`

합격:

- `execution_journal.phase='armed_wait_exit'`
- `metadata.detail='armed_entry_submit_timeout'`
- 대응하는 `armed_wait_enter` 가 선행
- `armed-stats` 에 aborted 계열 카운트/지연 반영

실패:

- detail 이 비어 있거나 다른 값
- `armed_wait_exit` 자체가 누락
- event_log 와 journal 의 outcome/detail 이 불일치

## 6. 현재 기준 추가 구현 필요 여부

현재 스키마와 엔드포인트만으로도 위 3개를 판정할 **최소 수집** 은 가능하다. 즉, 데이터 수집
자체를 시작하기 위해 선행 schema 변경이 필수는 아니다.

다만 아래 2개는 있으면 분석 품질이 더 좋아진다.

1. `scenario_id` 또는 `run_id` 를 `event_log.metadata` / `execution_journal.metadata` 에 함께 넣기
   - 현재는 `stock_code + 시간창` 으로 상관관계를 맞춰야 한다.
   - 같은 종목에서 장중 복수 timeout 이 발생하면 분석 난도가 올라간다.
2. 증거 export 스크립트 추가
   - 현재는 `psql` / HTTP endpoint / 로그 복사를 수동으로 수행해야 한다.
   - 반복 smoke 를 계획한다면 `scripts/collect_timeout_smoke.ps1` 같은 스크립트화가 적절하다.

이 두 항목은 권장 사항이고, 이번 문서의 범위는 "무엇을 수집해야 하는가" 와 "어디에 저장할 것인가"
까지다.

## 7. 권장 실행 순서

1. 장중 paper 세션 1회에서 시나리오 A, B, C 중 **관측 가능한 것부터** 수집
2. 수집 결과로 위 3개 판정
3. 수집이 애매하면 `run_id` tagging + export 스크립트를 보강
4. 그 다음 `#5.e` 또는 후속 actor 정리로 진행

