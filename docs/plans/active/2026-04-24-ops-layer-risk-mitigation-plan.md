# 2026-04-24 운영 레이어 잔여 리스크 수정 계획

## 배경

2026-04-24 DB 이력 검토 결과, 최근 수정은 2026-04-23 사고 원인인 체결 timeout, post-flat reconcile race, quote parser 오염, session wait 문제를 직접 막는 방향으로 반영됐다. 다만 운영 레이어에는 아직 자동매매 안정성을 낮추는 잔여 리스크가 남아 있다.

이 문서는 타 부서 리뷰를 반영해 기존 계획을 보수적으로 재작성한 것이다. 핵심 변경은 P0를 3단계로 쪼개고, `ManualHeld`, `PositionLockState`, 기존 `ExitResolutionSource::RestExecution`, 실제 preflight reason 값을 기준으로 계획을 정정한 점이다.

근거 문서:

- `docs/plans/done/2026-04-24-ops-layer-db-history-review.md`
- `docs/monitoring/2026-04-23-trading-postmortem.md`
- `CLAUDE.md` 의 WebSocket 정시 리셋 운영 지식

## 목표

- 장외 시간의 불필요한 WS 재연결과 balance/API polling을 줄이되, manual/pending/open 상태의 안전 감시는 유지한다.
- 청산 체결가 확정 경로를 강화하기 전에 기존 `RestExecution` 실패 원인을 먼저 확인한다.
- preflight 차단 원인을 실제 코드 reason 기준으로 집계한다.
- restart cleanup, race metadata 등 운영 로그의 신호 대비 노이즈 비율을 개선한다.

## 비목표

- OR/FVG/lock 진입 전략 자체를 변경하지 않는다.
- 손절, 익절, trailing stop 계산식을 변경하지 않는다.
- 실계좌 전환 정책을 변경하지 않는다.
- 장외 억제를 위해 manual intervention 탐지 능력을 희생하지 않는다.

## 우선순위 요약

| 우선순위 | 항목 | 목적 | 주요 파일 |
|---|---|---|---|
| P0-1 | 장외 WS backoff 확대만 선반영 | 저위험으로 재연결 폭주 완화 | `src/infrastructure/websocket/connection.rs`, `src/config.rs` |
| P0-2 | 상태 기반 장외 억제 | flat/off-hours만 억제하고 manual/pending/open은 유지 | `src/main.rs`, `src/strategy/live_runner.rs`, `src/presentation/routes/strategy.rs` |
| P0-3 | pre-open/휴장일 정책 정교화 | 09:00 정시 리셋과 휴장일 대응 | 세션 정책 helper, KIS 거래일 캘린더 |
| PRE-P1 | `RestExecution` 실패 원인 조사 | blind retry 방지 | DB 쿼리, `src/strategy/live_runner.rs` |
| P1 | 청산 체결가 확정 보강 | `balance_only`/`fill_price_unknown` 감소 | `src/strategy/live_runner.rs`, KIS order query adapter |
| P1 | preflight 운영 집계 | 실제 reason 기준 운영 가시성 확보 | monitoring route, `event_log` 쿼리 |
| P2 | restart cleanup 로그 정리 | 무의미한 orphan cleanup 노이즈 감소 | `src/strategy/live_runner.rs` |
| P3 | race metadata 통계 검증 | 45초 timeout 적정성 재평가 | DB 쿼리, monitoring 문서 |

## 공통 안전 요구사항

### Feature flag

장외 억제는 반드시 env 기반으로 끌 수 있어야 한다.

권장 설정:

- `KIS_OFF_HOURS_SUPPRESSION_ENABLED=false` 기본값으로 시작
- `KIS_OFF_HOURS_WS_BACKOFF_MS`
- `KIS_OFF_HOURS_BALANCE_SENTINEL_MS`
- `KIS_PRE_OPEN_CONNECT_AT`
- `KIS_REGULAR_OPEN_AT`
- `KIS_REGULAR_CLOSE_AT`
- `KIS_CLOSE_SETTLE_END_AT`

문제 발생 시 코드 롤백 없이 env 변경과 서버 재시작만으로 원복 가능해야 한다.

### 순수 함수 기반 세션 정책

시간 의존 로직은 테스트 가능한 순수 함수로 분리한다. `compute_session_wait_deadline`과 같은 패턴을 따른다.

예상 구조:

```rust
pub struct MarketSessionPolicy {
    pub suppression_enabled: bool,
    pub pre_open_connect_at: NaiveTime,
    pub regular_open_at: NaiveTime,
    pub regular_close_at: NaiveTime,
    pub close_settle_end_at: NaiveTime,
    pub off_hours_ws_backoff: Duration,
    pub off_hours_balance_sentinel: Duration,
}

pub struct SuppressionContext {
    pub execution_state: ExecutionState,
    pub manual_intervention_required: bool,
    pub position_lock_state: PositionLockState,
    pub has_active_position: bool,
    pub has_pending_order: bool,
    pub is_trading_day: bool,
}
```

### 억제 금지 상태

아래 조건 중 하나라도 true이면 WS 또는 balance 감시를 장외라는 이유만으로 완전 억제하지 않는다.

- `manual_intervention_required=true`
- `ExecutionState::ManualIntervention`
- `ExecutionState::EntryPending`
- `ExecutionState::EntryPartial`
- `ExecutionState::Open`
- `ExecutionState::ExitPending`
- `ExecutionState::ExitPartial`
- `PositionLockState::Pending`
- `PositionLockState::Held`
- `PositionLockState::ManualHeld`
- `active_positions`에 열린 포지션 존재
- 미체결 주문 존재

특히 `ManualHeld`는 `execution_state`만 보면 flat처럼 보일 수 있으므로 별도 입력으로 반드시 판단한다.

### 수동 HTS/MTS 주문 시나리오

flat + off-hours 상태에서도 사용자가 HTS/MTS로 직접 주문하면 hidden position이 생길 수 있다. 따라서 P0-2 이후에도 balance polling을 완전히 0으로 만들지 않고 저빈도 sentinel 조회를 유지한다.

권장:

- flat + off-hours: WS는 긴 backoff, balance는 15~30분 sentinel
- manual/pending/open/off-hours: 기존 안전 polling 유지

### 이벤트 중복 억제

장외 억제를 위해 새 이벤트를 무제한 기록하면 로그 노이즈가 다시 증가한다. 동일 이벤트는 종목/사유 기준 10분 내 1회만 기록한다.

대상 후보:

- `ws_reconnect_deferred_off_hours`
- `balance_poll_deferred_off_hours`
- `reconcile_skipped_off_hours_flat`

## 1. P0-1 장외 WS backoff 확대만 선반영

### 문제

2026-04-24 재시작 이후에도 장외 `ws_reconnect`가 1~2분 간격으로 발생했다. 단번에 상태 기반 억제를 넣으면 manual/pending/open 감시 회귀 위험이 크므로 1단계는 reconnect backoff 확대만 적용한다.

### 구현 계획

- 장외 시간 감지는 상태와 분리해 단순 시간 정책으로만 판단한다.
- 장외 `ws_reconnect` backoff를 5~15분 범위로 확대한다.
- 정규장 및 close settle window에서는 기존 2~4초 재연결을 유지한다.
- `KIS_OFF_HOURS_SUPPRESSION_ENABLED=false`여도 backoff만 별도 flag로 켤 수 있게 한다.

### 검증 기준

- 단위 테스트: 장외이면 backoff가 확대되고, 장중이면 기존 backoff 유지
- 실제 실행: 장외 최소 70분 관찰해 정시 리셋 포함 여부를 확인
- DB 검증: `ws_reconnect`가 시간당 30건대에서 한 자릿수로 감소

## 2. P0-2 상태 기반 장외 억제

### 문제

flat 상태의 장외 polling은 비용이 크지만, manual/pending/open 상태까지 억제하면 체결통보·잔고 변화·수동 청산을 놓칠 수 있다.

### 구현 계획

1. 세션 정책 helper 추가

필수 입력:

- 현재 시각
- 거래일 여부
- `ExecutionState`
- `manual_intervention_required`
- `PositionLockState`
- active position 존재 여부
- 미체결 주문 존재 여부

2. runner polling 조정

대상:

- `src/strategy/live_runner.rs`
- `src/main.rs`

변경:

- flat + off-hours + lock free + active position 없음 + pending order 없음일 때만 긴 sleep 또는 sentinel mode 사용
- manual/pending/open/held 상태는 기존 polling 유지
- 대기 모드는 `tokio::time::sleep(next_deadline - now)` 기반으로 구현하고, stop/reload 신호가 필요한 경우 `tokio::select!`로 깨운다.

3. balance/API polling 조정

대상:

- `src/presentation/routes/strategy.rs`
- `src/strategy/live_runner.rs`

변경:

- flat + off-hours에서는 reconcile을 skip하되, 15~30분 sentinel balance 조회는 유지
- manual/pending/open/held 상태에서는 reconcile skip 금지
- skip 이벤트는 10분 단위로 downsample한다.

### 회귀 리스크와 방어

- 장 마감 직후 15:25~15:45 청산 검증 window에서는 억제 금지
- 서버 재시작 직후 `active_positions` 또는 lock이 남아 있으면 억제 금지
- manual intervention 상태에서는 자동 청산은 금지하되 잔고 감시는 유지
- HTS/MTS 직접 주문 감지를 위해 sentinel balance polling 유지

### 검증 기준

- 단위 테스트: `ManualHeld`이면 off-hours라도 억제하지 않음
- 단위 테스트: `PositionLockState::Pending/Held/ManualHeld`이면 억제하지 않음
- 단위 테스트: flat/free/no-position/no-order/off-hours이면 억제함
- 단위 테스트: 장외 수동 HTS 주문을 가정한 sentinel 주기 계산
- 실제 실행: 장외 70분 관찰에서 API 오류 증가 없음

## 3. P0-3 pre-open window와 휴장일 정교화

### 문제

KIS 서버는 매 정시(:00)에 WebSocket 연결을 리셋한다. 따라서 08:30 또는 08:45에 미리 연결해도 09:00 정각 리셋을 피할 수 없다. pre-open window의 목적은 09:00까지 계속 연결해 두는 것이 아니라, 장 시작 전 인증·구독 경로를 warm-up하고 09:00 리셋 직후 빠르게 복구하는 것이다.

또한 평일 09:00~15:30만 전제하면 공휴일·임시휴장일에 불필요한 연결과 polling이 반복된다.

### 구현 계획

- pre-open 시작 시각은 env로 노출하고 기본값은 08:30 또는 08:45 중 운영 선택 가능하게 한다.
- 09:00 정시 리셋은 정상 경로로 간주하고, 09:00~09:01 재연결 상태를 별도 warn 폭주로 보지 않는다.
- OR 시작 직후 첫 틱 누락을 방지하기 위해 09:00 리셋 이후 reconnect 완료 상태를 health metadata로 노출한다.
- 거래일 캘린더를 도입한다. 초기 구현은 KIS 국내지수 조회 또는 로컬 trading day cache 중 하나를 선택한다.
- 휴장일에는 regular window를 열지 않고 장외 정책을 적용한다.

### 검증 기준

- 단위 테스트: 휴장일이면 09:00에도 regular window가 열리지 않음
- 단위 테스트: 거래일 09:00 리셋 직후 reconnect grace 상태 계산
- 실제 실행: 09:00 전후 로그에서 정시 리셋 후 2~4초 reconnect가 정상 이벤트로 분류됨

## 4. PRE-P1 `RestExecution` 실패 원인 조사

### 문제

현재 코드는 이미 `ExitResolutionSource::RestExecution`을 가지고 있다. 따라서 신규 `RestOrderQuery` variant를 추가하면 중복이다. 2026-04-23 거래가 `balance_only`로 내려간 이유는 WS와 REST 체결조회가 모두 확정 실패했기 때문이다.

### 조사 계획

- 2026-04-23 `exit_market`, `exit_final_fill`, 관련 API 오류 로그를 시간순으로 재조회한다.
- REST 체결조회 실패 원인을 분류한다.
- HTTP/API 오류인지, 응답 파싱 실패인지, 조회 시점이 너무 빨랐는지, 부분 체결 평균가 계산 실패인지 구분한다.
- `execution_journal.metadata`, `event_log.metadata`, `order_log`의 order_no 기준으로 연결한다.

### 산출

- `RestExecution` 실패 원인 표
- 재시도 횟수와 간격 조정 필요 여부
- API endpoint 또는 parser 수정 필요 여부
- 부분 체결 평균가 계산 보강 필요 여부

## 5. P1 청산 체결가 확정 보강

### 전제

PRE-P1 조사 후 착수한다. blind retry만 추가하지 않는다.

### 구현 계획

- enum은 기존 `ExitResolutionSource::RestExecution`을 사용한다.
- REST 체결조회가 여러 체결 조각을 반환하면 수량 가중 평균가를 계산한다.
- REST 조회의 재시도 횟수와 간격은 PRE-P1 조사 결과로 정한다.
- balance-only fallback metadata는 필요한 필드만 추가한다.

권장 metadata:

- `rest_execution_attempts`
- `rest_execution_last_error`
- `balance_before`
- `balance_after`
- `balance_confirm_elapsed_ms`
- `partial_fill_count`
- `partial_fill_qty_sum`

중복 가능성이 큰 필드:

- `ws_received`: `exit_resolution_source`로 대부분 대체 가능하므로 필요 시에만 추가
- `exit_resolution_source`, `fill_price_unknown`: 이미 trade/journal에 저장되면 이벤트 metadata 중복은 선택 사항

### 검증 기준

- 단위 테스트: WS 체결가 우선, `RestExecution` 차선, `BalanceOnly` 최후 fallback 순서 검증
- 단위 테스트: 부분 체결 여러 건의 수량 가중 평균가 계산
- 단위 테스트: balance-only일 때 `fill_price_unknown=true` 유지
- 단기 운영 검증: 다음 청산 1건은 정성 확인만 수행
- 통계 검증: 최소 N=10 청산 이후 `balance_only` 비율 판단

## 6. P1 preflight 운영 집계

### 문제

운영 집계 항목은 실제 코드 reason 값과 일치해야 한다. 현재 preflight gate reason은 `poll_and_enter` 분기 기준 아래 값이다.

실제 reason:

- `subscription_health_stale`
- `quote_invalid`
- `spread_exceeded`
- `nav_gap_exceeded`
- `regime_unconfirmed`
- `preflight_gate_unknown`

현재 quote sanity invalid label:

- `nonpositive`
- `inverted`
- `off_market`

`missing_quote`는 현재 미구현이다. 추가하려면 `latest_quote_snapshot()` 또는 quote snapshot 없음 경로를 별도 감지하는 코드 변경으로 다룬다.

### 구현 계획

- monitoring route 또는 health payload에 실제 preflight reason count를 추가한다.
- reason 목록은 하드코딩하되 unknown bucket을 유지한다.
- `quote_invalid`는 metadata의 `quote_issue` 기준으로 3종을 집계한다.
- `missing_quote`는 별도 구현 항목으로 분리하고, paper fallback 경로와 충돌하지 않게 설계한다.

### alert 임계값

초기 임계값:

- 장 시작 후 60초 grace 기간에는 alert 제외
- `quote_invalid >= 3` and 표본 N>=20이면 warn
- `quote_invalid >= 10` and 표본 N>=20이면 critical
- 특정 종목 reason이 95% 이상 집중되더라도 N>=20일 때만 warn

### 검증 기준

- DB fixture 또는 mock event_log로 reason 집계 테스트
- 실제 DB 조회 결과가 `event_log` 원본 count와 일치
- 존재하지 않는 reason을 쿼리하지 않음

## 7. P2 restart cleanup 로그 정리

### 문제

`execution_journal`에 `orphan_restart_cleanup`이 16건 존재한다. holdings=0이면 안전한 no-op인데, 현재는 운영자가 중요한 복구 이벤트와 구분하기 어렵다.

### 구현 계획

- 신규 event_type을 늘리지 않고 기존 event_type을 유지한다.
- no-op 여부는 metadata의 `cleanup_action="noop"`으로 구분한다.
- 실제 stale DB row 제거 또는 실제 보유 복구가 있을 때만 `cleanup_action="recovered"` 또는 `cleanup_action="removed_stale_position"`으로 기록한다.
- event_log severity는 `info`까지만 사용한다. 현재 스키마와 운영 관례상 `debug` severity는 사용하지 않는다.

추가 metadata:

- `holding_qty`
- `active_position_exists`
- `stale_db_position_removed`
- `cleanup_action`

### 검증 기준

- 단위 테스트: no-op restart는 warn/critical 이벤트를 만들지 않음
- 단위 테스트: 실제 stale position 제거는 cleanup metadata에 action을 남김

## 8. P3 race metadata 통계 검증

### 문제

과거 `cancel_fill_race_detected`에는 timing metadata가 없어 45초 timeout의 적정성을 통계적으로 판단하기 어렵다. 최근 수정 이후부터는 신규 metadata가 쌓인다.

### 운영 기준

- N<20이면 p90 판단 금지, 개별 사례만 기록
- `p90 < entry_fill_timeout_ms * 0.8`이면 유지
- `entry_fill_timeout_ms * 0.8 <= p90 < entry_fill_timeout_ms * 0.9`이면 관찰 지속
- `p90 >= entry_fill_timeout_ms * 0.9`이면 timeout 또는 주문 방식 재검토
- 45초 이후에도 race가 반복되면 timeout 상향보다 주문 방식 또는 cancel/fill reconciliation 강화 우선

### 검증 쿼리

```sql
select event_time, stock_code, metadata
from event_log
where event_type = 'cancel_fill_race_detected'
order by event_time desc
limit 20;
```

```sql
select count(*) as n,
       percentile_cont(0.9) within group (order by order_to_fill_ms) as p90_ms,
       max(order_to_fill_ms) as max_ms
from trades
where order_to_fill_ms is not null
  and order_to_fill_ms > 0;
```

## 작업 순서

1. P0-1 장외 WS backoff 확대와 feature flag 추가
2. P0-1 장외 70분 실제 실행 검증
3. PRE-P1 `RestExecution` 실패 원인 조사 문서 작성
4. P1 preflight 집계 reason 목록 정정 및 구현
5. P0-2 상태 기반 억제 helper 설계 및 단위 테스트
6. P0-2 runner/reconcile polling 억제 적용
7. P0-3 pre-open 정시 리셋 grace와 휴장일 정책 설계
8. P1 청산 체결가 확정 보강
9. P2 restart cleanup 로그 정리
10. P3 race metadata는 N>=20 이후 통계 평가

## PR 분리 제안

1. PR 1: P0-1 장외 WS backoff 확대와 env rollback
2. PR 2: PRE-P1 조사 결과 문서와 preflight 집계 reason 정정
3. PR 3: P0-2 상태 기반 장외 억제
4. PR 4: P0-3 pre-open/휴장일 정책
5. PR 5: P1 청산 체결가 확정 보강
6. PR 6: P2 로그 노이즈 정리

## 완료 조건

- `cargo test` 통과
- P0-1 적용 후 장외 70분 관찰에서 `ws_reconnect`가 시간당 한 자릿수로 감소
- feature flag off 시 기존 재연결 동작으로 즉시 복귀
- P0-2 적용 후 `ManualHeld`, `Pending`, `Held`, open/pending/manual 상태에서 polling 억제 없음
- 장외 flat/free 상태에서도 sentinel balance polling으로 HTS/MTS 직접 주문 탐지 여지를 유지
- preflight 집계가 실제 reason 값과 일치
- PRE-P1 조사 없이 청산 체결가 경로를 임의로 바꾸지 않음
