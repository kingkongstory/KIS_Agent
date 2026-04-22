# 2026-04-19 실행 계층 후속 구현 계획

## 문서 목적

이 문서는 `2026-04-18-execution-timing-implementation-plan.md` 이후 실제 구현과 재검토 결과를 반영하여,
**다음 단계에서 무엇을 새로 구현해야 하는지**를 명확히 적는 후속 계획서다.

이번 문서의 핵심은 아래 두 가지다.

- 이미 구현된 항목을 다시 구현 대상으로 오해하지 않도록 기준선을 고정한다.
- 아직 남아 있는 구조 변경 작업을 별도 범위로 분리해 구현자가 과도한 일괄 리팩터링을 하지 않도록 한다.

이 문서는 특히 타부서 검토 의견까지 반영한 **다음 단계 작업 경계 문서**다.

## 현재 기준선

다음 항목은 이번 시점 기준으로 **이미 구현되었거나 구현 완료로 간주하는 기준선**이다.

### 1. `EntryPending` 영속화와 재시작 복구

- `active_positions.pending_entry_order_no`
- `active_positions.pending_entry_krx_orgno`
- `execution_journal.entry_submitted`
- `execution_journal.sent_at / ack_at / first_fill_at / final_fill_at`
- `restore_entry_pending_state()`

즉, 다음 단계에서는 더 이상 "`EntryPending` durable recovery 자체가 없다"는 전제로 일하면 안 된다.

### 2. `SignalArmed` 이후 짧은 감시 구간

- `ArmedWaitOutcome`
- `armed_wait_for_entry()`
- `LiveRunnerConfig.armed_watch_budget_ms`
- `LiveRunnerConfig.armed_tick_check_interval_ms`
- `trade_tx` 기반 감시 + paced timer

즉, 다음 단계의 목표는 "`SignalArmed` 감시가 전혀 없다"가 아니라,
**현재 구현된 armed watch를 더 운영 친화적으로 다듬고 책임을 분리하는 것**이다.

### 3. ETF gate 주입 지점

- `ExternalGates`
- `LiveRunner.external_gates`
- `with_external_gates()`
- `refresh_preflight_metadata()`의 외부 gate 반영

즉, 다음 단계에서는 execution 계층 내부에서 NAV/레짐을 계산하는 것이 아니라,
**외부 계산 결과를 누가 만들고 언제 주입할지**를 구현해야 한다.

### 4. 재시작 복구 수량 정합성

- `enter_manual_after_restart_with_position()` 수량 버그는 수정되었다.
- `build_restart_restored_position(... quantity=holdings_qty ...)` 기준으로 실제 잔고 수량을 포지션에 넣는다.

따라서 다음 단계는 이 버그를 재수정하는 것이 아니라,
이 정합성이 계속 유지되도록 테스트와 구조를 강화하는 단계다.

### 5. 실행 상태머신의 중간 단계 단일화

- `ExecutionState` enum
- `transition_execution_state()`
- `EntryPending / EntryPartial / Open / ExitPending / ExitPartial / ManualIntervention`

다만 이 상태머신은 아직 `live_runner.rs` 안에 남아 있으므로,
**최종 구조 분리는 아직 끝나지 않았다.**

## 타부서 검토 의견 반영 결과

타부서 검토 의견을 다음 단계 계획에 반영하면 아래처럼 정리된다.

### 지적 #1: `execution_actor.rs` 분리 미흡

- 맞는 지적이다.
- 다만 이번 단계에서 대공사로 한 번에 처리하지 않는다.
- 다음 단계에서는 "`execution actor` 도입을 위한 분리 작업"을 **단계적으로** 수행한다.

### 지적 #2: `SignalArmed` 후 즉시 `execute_entry`

- 이 지적은 현재 기준선에서는 해소된 것으로 본다.
- 다만 다음 단계에서는 armed watch의 책임과 관측 지표를 더 정리해야 한다.

### 지적 #3: `EntryPending` durable recovery 약함

- 현재 기준선에서는 핵심 경로가 구현되었다.
- 다음 단계에서는 이 경로를 운영/테스트/관측 기준으로 보강한다.

### 지적 #4: ETF gate placeholder

- 현재는 placeholder가 아니라 **주입 지점**이 준비된 상태다.
- 다음 단계는 외부 계산기와 runtime wiring 구현이다.

### 지적 #5: 재시작 복구 수량 버그

- 현재 기준선에서 수정되었다.
- 다음 단계는 재시작 복구 매트릭스 테스트와 운영 문서 보강이다.

## 이번 문서에서 정의하는 다음 단계 목표

다음 단계의 목표는 전략 로직 자체를 바꾸는 것이 아니다.

정확한 목표는 아래 4개다.

1. ETF gate 값을 실제 생산하는 외부 계층을 붙인다.
2. armed watch를 운영 가능한 수준으로 다듬는다.
3. 재시작 복구와 journal을 운영/테스트 관점에서 완성한다.
4. `live_runner`에 남은 실행 상태머신 책임을 단계적으로 분리한다.

## 이번 단계의 비목표

아래는 이번 문서 기준 **다음 단계에서 하지 않는 일**이다.

- ORB+FVG 전략 철학 자체를 재설계하는 일
- execution layer 안에서 NAV/레짐/활성 ETF를 직접 계산하는 일
- `live_runner.rs`를 한 번에 전부 갈아엎는 빅뱅 리팩터링
- 백테스트 로직과 라이브 로직을 동시에 크게 재설계하는 일

## 작업 스트림

### 1. ETF gate producer 연결

현재는 `ExternalGates` 주입 지점만 있다.
다음 단계에서는 아래 생산자를 붙여야 한다.

- `index_regime`
- `active_instrument`
- `nav_gate_ok`
- `subscription_health_ok`
- 필요 시 `spread_ok` 외부 override

구현 원칙은 아래와 같다.

- 계산은 전략/데이터 계층이 한다.
- 적용은 실행 계층이 한다.
- `ExternalGates`는 계산기가 아니라 **주입 인터페이스**다.

필수 구현 항목:

- 외부 gate 업데이트 서비스 또는 manager 도입
- gate stale 기준 명시
- real mode에서 stale/fail 시 신규 진입 fail-close
- paper mode에서는 필요 시 완화 가능하되 기본 정책을 문서화
- `event_log`에 gate snapshot 또는 gate change 기록

주의:

- execution actor 또는 `live_runner`가 NAV를 직접 계산하면 안 된다.
- ETF 규칙 초안의 계산 책임과 실행 책임을 섞으면 안 된다.

관련 문서:

- `docs/strategy/etf-leverage-inverse-rule-draft.md`

### PR #1.e 결정값

이 항목은 "어떤 소스를 붙일지"가 정해져야 구현이 시작된다.
다음 값을 **v1 구현 기준**으로 고정한다.

- `index_regime`의 권위 소스는 KIS **REST 지수 분봉**이다.
- 우선순위는 `업종 분봉조회`이며, KOSPI200 1분 OHLC 확보가 불가능한 경우에만 `국내업종 시간별지수(분)`을 fallback 으로 검토한다.
- `H0UPCNT0` 국내지수 실시간체결은 **권위 소스가 아니라 보조 소스**다.
- `H0UPCNT0`는 armed 상태의 빠른 재확인, intrabar drift 감시, stale 감지, 장중 regime invalidation 보조에만 사용한다.
- `H0UPPGM0` 국내지수 실시간프로그램매매는 **v1에서 hard gate 로 쓰지 않는다.**
- `H0UPPGM0`는 event_log 보조지표 또는 regime confidence 보강용으로만 사용한다.
- `H0UPANC0` 국내지수 실시간예상체결은 v1 구현 범위에서 제외한다.
- Yahoo 등 외부 시세는 **실행 gate 계산에 사용하지 않는다.**

- `nav_gate_ok`의 런타임 권위 소스는 `H0STNAV0` 국내ETF NAV추이다.
- 재기동/재구독 직후 bootstrap 과 backfill 은 `NAV 비교추이(분)`을 사용한다.
- `NAV 비교추이(종목)`과 `ETF/ETN 현재가`는 point-in-time sanity check 용도로만 사용한다.
- real mode 에서는 `H0STNAV0` stale 또는 재동기화 실패 시 신규 진입을 fail-close 한다.
- paper mode 에서는 동일 상황을 로그/메트릭으로 남기되, 완화 여부는 config 로 분리한다.

레짐 판정 v1은 아래처럼 단순하게 고정한다.

1. `09:15` 이전에는 항상 `Neutral`이다.
2. `09:15` 이후에는 KOSPI200 확정 5분봉 기준으로만 레짐을 판정한다.
3. `close > index_or_high` 이고 봉 방향이 양봉이면 `Bullish`다.
4. `close < index_or_low` 이고 봉 방향이 음봉이면 `Bearish`다.
5. 그 외는 `Neutral`이다.
6. `Bullish -> active_instrument=122630`, `Bearish -> active_instrument=114800`, `Neutral -> None` 으로 고정한다.
7. 상방/하방이 짧은 시간 안에 교차하면 `Neutral`로 되돌린다. v1에서는 "최근 2개 확정 5분봉 내 반대 방향 돌파 발생"을 교차 기준으로 사용한다.
8. 프로그램매매는 v1에서 레짐 성립의 필요조건이 아니다.

구현 주의:

- WS 하나로 5분봉을 직접 만들고 그 결과를 권위 레짐으로 사용하지 않는다.
- `index_regime`는 REST 확정봉이 만든 값이고, WS 는 그 값을 더 빨리 무효화하거나 보조 설명을 붙이는 역할만 한다.
- `nav_gate_ok`는 최신 NAV 시각과 ETF 현재가를 함께 저장해서 계산한다.
- NAV 괴리 기본 임계값은 ETF 규칙 초안 기준대로 `0.50%`를 사용한다.
- v1에서는 "최근 3개 NAV 업데이트 연속 확대" 조건을 보조 경고로만 기록하고, hard block 은 `0.50%` 초과와 stale fail-close 부터 시작한다.

### 2. armed watch 운영 보강

현재 armed watch는 구현되어 있다.
다음 단계는 이 기능을 운영 친화적으로 다듬는 것이다.

필수 구현 항목:

- armed 진입/해제 사유를 `event_log`와 `execution_journal`에 더 명확히 남기기
- armed 구간 latency 측정
- `drift exceeded`, `preflight failed`, `cutoff reached`, `stopped` 통계를 남기기
- 동일 signal에 대한 armed 루프 중복 생성 방지
- stop/manual/degraded 전이 시 armed watcher 즉시 종료 보장

권장 구현 항목:

- `trade_tx` 감시를 별도 helper 또는 actor로 분리
- armed watcher와 signal 탐색 루프의 책임 분리
- 실거래에서 `200ms` budget과 실제 체감 지연의 차이를 측정할 지표 추가

중요:

- 이 단계의 목표는 armed watch를 "다시 구현"하는 것이 아니다.
- 이미 있는 armed watch를 **관측 가능하고 유지보수 가능한 구조**로 만드는 것이다.

### 3. 재시작 복구 매트릭스 완성

현재 `EntryPending`과 `ExitPending` 복구 경로는 들어갔다.
다음 단계에서는 이를 운영 기준으로 완성해야 한다.

필수 구현 항목:

- 재시작 시나리오 테스트 매트릭스 추가
- `Flat`
- `EntryPending`
- `EntryPartial`
- `Open`
- `ExitPending`
- `ExitPartial`
- `ManualIntervention`

각 상태에서 최소한 아래를 검증한다.

- balance 0 / balance > 0
- execution 조회 가능 / 불가
- WS 통보 유무
- stale pending order 존재 / 미존재

필수 구현 항목:

- 재시작 복구 결과를 `event_log`에도 명시
- `manual_intervention` 진입 시 운영자가 봐야 하는 핵심 메타를 표준화
- stale pending entry 정리 기준을 문서화

권장 구현 항목:

- 재시작 복구 전용 integration-style test helper 도입
- 복구 결과 요약 API 또는 운영용 상태 조회 확장

### 4. execution journal taxonomy 정리

현재 journal은 이전보다 좋아졌지만, phase 체계와 운영 쿼리 관점에서 더 정리할 필요가 있다.

필수 구현 항목:

- phase 명칭을 문서와 코드에서 완전히 맞추기
- `entry_submitted`
- `entry_acknowledged`
- `entry_partial`
- `entry_final_fill`
- `entry_cancelled`
- `entry_restart_auto_clear`
- `entry_restart_manual_intervention`
- `exit_submit_failed`
- `exit_submit_rejected`
- `exit_final_fill`
- `exit_retry_*`

- 운영 쿼리 예시를 문서에 추가
- `sent_at`, `ack_at`, `first_fill_at`, `final_fill_at` 활용 기준 명시
- 어떤 phase가 주문 단위이고 어떤 phase가 복구 단위인지 구분

권장 구현 항목:

- `execution_id`와 `broker_order_id` 사용 규칙을 문서화
- signal 기준 조회와 order 기준 조회 둘 다 가능하도록 인덱스/조회 패턴 정리

### 5. execution actor 도입 준비 단계

이 항목은 다음 단계의 핵심이지만, 빅뱅 리팩터링으로 처리하지 않는다.

### 이번 단계에서 해야 하는 것

- `execution_actor.rs` 또는 동등 모듈의 최소 골격 추가
- actor가 소유해야 할 책임을 명시적으로 분리
- 주문 제출
- 체결 확인
- timeout
- 재시작 복구 결정
- execution_state 전이

- `live_runner`에서 분리 가능한 helper부터 이동
- signal 탐색과 실행 상태 전이 경계 정리

### 이번 단계에서 하지 않는 것

- `live_runner` 전체를 한 번에 actor로 옮기기
- 전략 계산까지 actor에 합치기
- NAV/레짐 계산까지 actor에 넣기

### 구현 원칙

- actor는 주문/체결/상태 전이 책임만 가진다.
- 전략 계층은 signal과 gate 계산 책임만 가진다.
- actor는 외부 gate 값을 소비만 한다.

## 권장 PR 순서

1. ETF gate producer 연결
2. armed watch 운영 보강과 메트릭 추가
3. 재시작 복구 매트릭스 테스트 + journal taxonomy 정리
4. execution actor 최소 골격 추가
5. `live_runner`에서 execution helper 일부 이동

이 순서를 지키는 이유는 아래와 같다.

- gate producer와 armed watch는 현재 구조 위에서도 안전하게 추가 가능하다.
- 복구 테스트와 journal 정리는 actor 분리 전에 안전망이 된다.
- actor 분리는 그 다음에 해야 회귀를 잡기 쉽다.

## 수용 기준

다음 단계 완료 기준은 아래와 같다.

- ETF gate 값이 외부에서 실제로 주입되고 진입 차단에 반영된다.
- real mode에서 gate stale 또는 불일치 시 신규 진입이 fail-close 된다.
- armed watch 종료 사유가 로그/지표로 남는다.
- 재시작 복구 테스트가 상태별 매트릭스로 존재한다.
- `EntryPending`과 `ExitPending` 복구가 테스트로 고정된다.
- journal 시각 필드 활용 규칙이 문서와 코드에서 일치한다.
- `execution_actor.rs` 또는 동등 구조가 최소 골격 수준으로 도입된다.

## 구현자 금지사항

- "`SignalArmed`는 이미 있으니 더 할 일 없다"라고 해석하지 않는다.
- "`ExternalGates`가 있으니 ETF gate도 끝났다"라고 해석하지 않는다.
- "`execution actor`는 나중 일"이라며 계속 `live_runner`에만 책임을 쌓지 않는다.
- 반대로 "`actor`가 중요하니 지금 전부 옮긴다"는 식의 대공사를 하지 않는다.
- ETF 규칙 초안의 계산 책임을 execution layer 안으로 끌고 오지 않는다.

## 한 줄 요약

다음 단계는 전략을 다시 짜는 단계가 아니다.
현재 구현된 `EntryPending` 복구, armed watch, external gates 주입 지점을 **운영 가능한 구조와 책임 분리**로 끌어올리는 단계다.
