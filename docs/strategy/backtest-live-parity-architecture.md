# 백테스트-라이브 정합 아키텍처 설계

작성일: 2026-04-16  
대상: `orb_fvg` 현재 구현, 차기 `orb_vwap_pullback` 포함 공통 실행 구조  
관련 문서:
- `docs/monitoring/2026-04-15-analysis.md`
- `docs/monitoring/pr1-fix-plan.md`
- `docs/monitoring/strategy-redesign-plan.md`
- `docs/strategy/ORB_FVG_STRATEGY.md`

## 1. 목적

이 문서의 목적은 현재 프로젝트의 가장 큰 구조적 문제인

- 백테스트와 라이브가 서로 다른 진입/체결/청산 규칙을 사용하고 있고
- 그 결과 백테스트 수익률이 실거래 기대값의 신뢰 가능한 근거가 되지 못하는 상황

을 해소하기 위한 공통 아키텍처를 정의하는 것이다.

핵심 목표는 다음 하나로 요약된다.

> 같은 전략이면 백테스트, 리플레이, 라이브가 가능한 한 같은 상태기계와 같은 실행 정책을 사용해야 한다.

즉, 목표는 "현재 전략이 좋은가"를 먼저 증명하는 것이 아니라,  
"현재 전략이 백테스트와 라이브에서 왜 다르게 나오는지 설명 가능한 구조"를 만드는 것이다.

## 2. 현재 가장 큰 문제

현재 구현의 핵심 문제는 전략 로직의 성능 이전에, **동일한 전략이 동일한 방식으로 실행되지 않는 것**이다.

### 2.1 진입/체결 모델 불일치

- 백테스트 `orb_fvg.rs`는 FVG zone 진입 시 `gap.mid_price()` 체결을 가정한다.
- 라이브 `live_runner.rs`는 zone 진입 시 `gap.top` 또는 `gap.bottom`으로 패시브 지정가를 내고, 최대 30초 대기 후 취소/재검증한다.
- 라이브는 실제 체결가 기준으로 SL/TP를 재평행이동한다.

이 차이로 인해 다음 값이 모두 달라진다.

- intended entry
- 실제 R
- TP 가격
- 체결 확률
- 미체결 확률
- 시간 경과에 따른 취소 비율
- 최종 손익 분포

즉, 현재 백테스트는 "전략의 기대값"이 아니라 "다른 실행 우주에서의 기대값"에 가깝다.

### 2.2 Multi-Stage 선택 규칙 불일치

- 백테스트는 하루 전체를 보고 5m/15m/30m 중 가장 빠른 첫 진입이 나오는 stage를 사전 선택한다.
- 라이브는 매 사이클 stage를 순차 스캔하고 먼저 발견된 신호를 채택한다.

겉보기에는 둘 다 "선착순"처럼 보이지만, 실제로는

- 하나는 미래를 알고 stage를 고르는 사후 선택이고
- 다른 하나는 현재 시점에서만 판단하는 순차 탐색

이다.

### 2.3 청산 로직 중복 구현

- 백테스트는 `simulate_exit()`로 청산한다.
- 라이브는 `manage_position()`로 청산한다.

현재 두 로직은 유사하지만 별도 구현이므로, 추후 수정 시 한쪽만 바뀌는 회귀가 발생하기 쉽다.

### 2.4 상태 복구/재시작 경로가 백테스트에 없음

실거래는 다음 개념을 가진다.

- position lock
- search_after
- manual intervention
- cancel/fill race
- degraded / health gate

백테스트는 이 중 일부만 근사한다. 따라서 사고 날짜 분석 시 "전략이 틀린 것인지 실행이 틀린 것인지"를 분리하기 어렵다.

## 3. 설계 원칙

1. 전략 신호 탐지와 주문 실행은 분리한다.
2. 백테스트는 신호 재현이 아니라 실행 포함 기대값 재현을 목표로 한다.
3. 현재 백테스트는 삭제하지 않고 `legacy baseline`으로 보존한다.
4. 새 공통 구조는 순수 계산 계층과 I/O 계층을 명확히 분리한다.
5. 라이브에서 이미 필요한 안전장치(`search_after`, `position_lock`, `manual_intervention`)는 parity backtest와 replay에서도 관찰 가능해야 한다.
6. 새 전략 도입보다 먼저 현재 전략의 백테스트-라이브 정합을 확보한다.

## 4. 목표와 비목표

### 4.1 목표

- 현재 `orb_fvg` 전략을 기준으로 백테스트와 라이브의 상태 전이 차이를 줄인다.
- `SignalEngine + ExecutionPolicy + PositionManager`를 공통 모듈로 분리한다.
- `legacy backtest`, `parity backtest`, `replay`, `live` 네 경로가 같은 핵심 규칙을 공유하게 만든다.
- 차후 `orb_vwap_pullback` 같은 새 전략도 같은 실행 엔진 위에 올릴 수 있게 한다.

### 4.2 비목표

- 이 문서는 곧바로 새 전략의 진입 필터를 설계하는 문서가 아니다.
- 이 문서는 장중 운영 정책을 직접 결정하는 문서가 아니다.
- 이 문서는 지금 당장 실행 정책을 시장성 지정가로 바꾸는 문서가 아니다.

중요한 순서는 다음과 같다.

1. 현재 live 정책을 기준으로 parity 구조를 먼저 만든다.
2. parity가 잡힌 뒤 실행 정책을 개선한다.
3. 그 다음 새 전략을 올린다.

## 5. 현재 백테스트는 어떻게 보존할 것인가

현재 백테스트 로직은 없애면 안 된다. 다만 용도를 바꿔야 한다.

### 5.1 보존 대상

- `src/strategy/orb_fvg.rs`
- `src/strategy/backtest.rs`
- 현재 보고서 산출 방식
- 현재 `mid_price` 기반 진입 가정
- 현재 multi-stage 사전 선택 방식

### 5.2 새 역할

현재 백테스트는 앞으로 **실거래 승인 기준**이 아니라 **legacy baseline** 역할을 맡는다.

용도는 세 가지다.

1. 기존 전략 가설의 역사적 기준선 보존
2. 리팩터링 중 신호 탐지 회귀 감지
3. parity backtest와의 차이 분석 기준

### 5.3 보존 원칙

- `legacy` 경로는 결과가 변하지 않도록 가능한 한 동결한다.
- 보고서에는 `strategy_version`, `execution_version`, `simulation_mode`를 함께 기록한다.
- 비교용 고정 날짜 세트를 유지한다.
  - `2026-04-14`
  - `2026-04-15`
  - 최근 정상 장세 샘플 몇 일

## 6. 목표 아키텍처

핵심 구조는 다음과 같다.

```text
Strategy Definition
    -> SignalEngine
        -> SignalIntent
            -> ExecutionPolicy
                -> EntryPlan / FillDecision
                    -> PositionManager
                        -> ExitDecision

이 공통 계층 위에 4개 런타임이 붙는다.

1. Legacy Backtest
2. Parity Backtest
3. Replay
4. Live
```

### 6.1 계층 구분

#### A. Strategy Definition

전략 자체의 규칙과 파라미터를 정의한다.

- OR 수집 기준
- FVG 조건
- 방향 제약
- stage 사용 여부
- 신호 만료 규칙

이 계층은 "무슨 신호를 찾는가"를 담당한다.

#### B. SignalEngine

순수 함수 중심 계층이다. 입력 데이터와 상태를 받아 다음 신호를 계산한다.

책임:

- OR 계산
- FVG 탐지
- stage 선택
- `search_after` 반영
- `confirmed_side` 반영
- 다음 `SignalIntent` 산출

SignalEngine은 주문을 내지 않는다.

#### C. ExecutionPolicy

신호를 어떤 가격, 어떤 timeout, 어떤 drift budget, 어떤 체결 규칙으로 실행할지를 정의한다.

책임:

- intended entry 산정
- order price 산정
- drift guard 기준
- timeout 기준
- 취소 후 재검증 규칙
- fill resolution 규칙
- slippage 허용 규칙

ExecutionPolicy는 "어떻게 진입할 것인가"를 담당한다.

#### D. PositionManager

진입 후 포지션 관리와 청산 판단을 담당한다.

책임:

- 1R 도달 여부
- 본전 스탑
- trailing stop
- time stop
- end-of-day exit
- exit reason 판정

PositionManager는 백테스트와 라이브가 반드시 공유해야 하는 계층이다.

## 7. 공통 데이터 계약

공통화의 핵심은 함수 분리가 아니라 **같은 의미의 데이터 구조를 쓰는 것**이다.

### 7.1 SignalContext

신호 탐지 입력 컨텍스트.

예상 필드:

- `session_date`
- `strategy_id`
- `stage_candidates`
- `candles_1m`
- `candles_5m`
- `search_after`
- `trade_count`
- `confirmed_side`
- `market_regime_flags`

### 7.2 SignalIntent

SignalEngine 출력.

예상 필드:

- `side`
- `signal_time`
- `stage_name`
- `entry_zone_high`
- `entry_zone_low`
- `stop_anchor`
- `or_high`
- `or_low`
- `metadata`

중요: 여기에는 아직 실제 주문 가격이 들어가지 않는다.

### 7.3 EntryPlan

ExecutionPolicy가 `SignalIntent`로부터 만든 주문 계획.

예상 필드:

- `intended_entry_price`
- `order_price`
- `entry_mode`
- `max_entry_drift_pct`
- `timeout_ms`
- `cancel_on_timeout`
- `fill_recheck_mode`
- `risk_amount`
- `planned_stop_loss`
- `planned_take_profit`

### 7.4 FillResult

진입 주문의 최종 결과.

예상 필드:

- `status`
  - `Filled`
  - `Cancelled`
  - `Rejected`
  - `ManualIntervention`
- `filled_qty`
- `filled_price`
- `slippage`
- `fill_time`
- `broker_order_id`
- `resolution_source`
  - `ws_notice`
  - `daily_ccld`
  - `balance_recheck`
  - `historical_sim`

### 7.5 PositionSnapshot

포지션 상태의 공통 표현.

예상 필드:

- `side`
- `entry_price`
- `stop_loss`
- `take_profit`
- `quantity`
- `entry_time`
- `reached_1r`
- `best_price`
- `original_risk`
- `strategy_metadata`

### 7.6 ExitDecision

PositionManager가 내리는 청산 결정.

예상 필드:

- `should_exit`
- `reason`
- `exit_price_model`
- `effective_stop`
- `effective_take_profit`
- `decision_time`

## 8. 모듈별 책임과 파일 구조

아래는 권장 구조다. 한 번에 다 옮기기보다 단계적으로 도입한다.

### 8.1 권장 파일

- `src/strategy/signal_engine.rs`
- `src/strategy/execution_policy.rs`
- `src/strategy/position_manager.rs`
- `src/strategy/parity_backtest.rs`
- `src/strategy/replay.rs`
- `src/strategy/legacy_backtest.rs` 또는 기존 `backtest.rs` 유지
- `src/strategy/legacy_orb_fvg.rs` 또는 기존 `orb_fvg.rs` 유지

### 8.2 초기 단계에서의 현실적 배치

초기 리팩터링에서는 파일 rename보다 함수 추출이 우선이다.

권장 단계:

1. `orb_fvg.rs` 안에서 Signal 관련 순수 로직만 추출
2. `live_runner.rs` 안에서 Position 관리 순수 로직만 추출
3. 새 `execution_policy.rs` 도입
4. 이후 파일 분리

즉, 처음부터 대규모 rename을 하지 않는다.

## 9. SignalEngine 설계

### 9.1 역할

SignalEngine은 다음 질문에 답해야 한다.

- 지금 시점에 진입 가능한 신호가 있는가
- 있다면 어떤 stage의 어떤 방향 신호인가
- 이 신호가 현재 상태(`confirmed_side`, `search_after`, `trade_count`)와 양립 가능한가

### 9.2 포함해야 할 현재 규칙

Parity 1차 목표는 현재 live 동작을 재현하는 것이다. 따라서 우선 아래 규칙을 그대로 가져간다.

- 현재 multi-stage OR 후보 생성 방식
- 현재 `search_after` 처리
- 현재 `confirmed_side` 처리
- 현재 FVG expiry 규칙
- 현재 stage 순회 순서

이후 새 전략 전환 시 여기만 교체하면 된다.

### 9.3 꼭 분리해야 하는 이유

현재는

- 백테스트의 `scan_and_trade()`
- 라이브의 `poll_and_enter()`

가 유사하지만 다른 형태로 존재한다. 이 상태로는 한쪽 수정이 다른 쪽에 자동 전파되지 않는다.

## 10. ExecutionPolicy 설계

ExecutionPolicy는 이번 설계의 중심이다.

### 10.1 왜 가장 중요하나

현재 백테스트-라이브 괴리의 최대 원인은 진입/체결 규칙 차이이기 때문이다.

같은 신호라도

- `mid_price`로 들어갔다고 계산하는지
- `gap.top/bottom` 패시브 지정가를 내는지
- 시장성 지정가를 쓰는지
- 몇 초 기다렸다 취소하는지
- drift가 어느 정도면 포기하는지

에 따라 결과가 완전히 달라진다.

### 10.2 1차 parity 정책

첫 번째 공통 정책은 **현재 live에 최대한 맞춘 정책**이어야 한다.

예시 이름:

- `PassiveTopBottomTimeout30s`

규칙:

- 신호는 zone 진입 시 발생
- intended entry는 현재 live 기준 가격 규칙 사용
- order price는 현재 live와 동일
- timeout은 30초
- cancel 후 체결 진실값 재확인
- fill 후 SL/TP 재평행이동

중요: parity 1차에서는 더 나은 정책이 아니라 **현재 실제 정책의 명문화**가 목적이다.

### 10.3 2차 정책

Parity가 확보된 뒤에야 다음 정책으로 넘어간다.

- `MarketableLimitWithSlippageBudget`
- `ReversalConfirmedEntry`
- `FirstPullbackOnly`

즉, ExecutionPolicy는 "리팩터링 대상"이자 동시에 "전략 개선 확장점"이다.

## 11. PositionManager 설계

### 11.1 책임

PositionManager는 포지션 진입 이후의 상태 전이를 책임진다.

- 1R 도달
- 본절 이동
- trailing
- time stop
- end-of-day
- exit reason 부여

### 11.2 공통화 대상

현재 백테스트 `simulate_exit()`와 라이브 `manage_position()`는 다음 규칙을 사실상 공유한다.

- TP 우선
- 1R에서 BE
- trailing
- time stop
- end-of-day

이 공통 규칙은 pure function으로 묶어야 한다.

### 11.3 라이브 특수사항 처리

라이브에서만 필요한 것은 별도 adapter가 담당한다.

- 현재가 수신
- broker-side TP 주문
- WS 체결통보
- cancel/fill race 방어

PositionManager 본체는 "청산 결정을 어떻게 내리는가"까지만 책임진다.

## 12. Legacy / Parity / Replay 3계층

### 12.1 Legacy Backtest

역할:

- 기존 성능 기준선 보존
- 회귀 감지
- 역사적 비교 자료

특징:

- 현재 `mid_price`/현행 multi-stage 사전선택 유지 가능
- 실거래 승인 기준으로 사용하지 않음

### 12.2 Parity Backtest

역할:

- 라이브와 같은 신호/진입/청산 정책으로 기대값 계산
- 전략 수정 전후 비교
- 실행 모델 검증

특징:

- SignalEngine 공유
- ExecutionPolicy 공유
- PositionManager 공유
- historical candles/ticks 기반 fill simulator 사용

### 12.3 Replay

역할:

- 사고 날짜와 실제 장중 로그를 같은 상태기계로 재생
- live 의사결정 사후 설명
- event_log와 DB 상태 검증

특징:

- 실제 order/event timeline 입력
- WS / REST / balance 재검증 경로 반영
- `search_after`, `position_lock`, `manual_intervention` 관찰 가능

## 13. 구현 단계

### Phase 0. Legacy baseline 동결

- 현재 `orb_fvg` 백테스트 결과를 legacy 기준선으로 보존
- 보고서에 `strategy_version=legacy_orb_fvg`
- 고정 비교 날짜 세트 저장

산출물:

- 날짜별 거래 리스트
- 요약 리포트
- 비교 스냅샷

### Phase 1. 공통 타입 도입

- `SignalIntent`
- `EntryPlan`
- `FillResult`
- `PositionSnapshot`
- `ExitDecision`

이 단계에서는 동작을 바꾸지 않는다.

### Phase 2. SignalEngine 추출

원천:

- `orb_fvg.rs`의 `scan_and_trade()`
- `live_runner.rs`의 `poll_and_enter()` 신호 탐지 부분

목표:

- 같은 입력이면 같은 `SignalIntent`를 만들게 함

### Phase 3. PositionManager 추출

원천:

- `simulate_exit()`
- `manage_position()`

목표:

- 백테스트와 라이브 청산 규칙을 하나의 pure decision 함수로 통합

### Phase 4. ExecutionPolicy 1차 도입

원천:

- 현재 live `execute_entry()`의 intended entry / limit / timeout / cancel / recheck 규칙

목표:

- parity backtest가 현재 live 실행 규칙을 재현

### Phase 5. Parity Backtest 구축

- `SignalEngine + ExecutionPolicy + PositionManager` 조합
- 기존 legacy 백테스트와 병행 유지

### Phase 6. Replay 구축

- 사고 날짜를 재생
- event_log / active_positions / trades 정합성 확인

### Phase 7. 새 전략 전환

이 단계에서만

- 15분 OR 단일화
- VWAP pullback
- reversal confirmation
- marketable limit

같은 새 정책을 도입한다.

## 14. 검증 계획

### 14.1 단위 테스트

- SignalEngine가 같은 캔들 입력에서 같은 `SignalIntent`를 반환하는지
- ExecutionPolicy가 같은 signal 입력에서 같은 `EntryPlan`을 만드는지
- PositionManager가 같은 position/path 입력에서 같은 `ExitDecision`을 내리는지

### 14.2 Golden day 테스트

고정 날짜:

- `2026-04-14`
- `2026-04-15`
- 정상 장세 2~3일

검증 항목:

- signal count
- first signal time
- stage selection
- intended entry
- exit reason
- cumulative pnl

### 14.3 Parity 테스트

비교 축:

1. legacy backtest
2. parity backtest
3. replay
4. live logs

이 네 결과가 같은 날짜에서 어떻게 다른지 설명 가능한 표를 만들어야 한다.

### 14.4 운영 검증

라이브/모의투자에서 반드시 저장할 항목:

- `signal_time`
- `stage_name`
- `entry_mode`
- `intended_entry_price`
- `order_price`
- `actual_fill_price`
- `fill_source`
- `exit_reason`
- `search_after`
- `manual_intervention_reason`

## 15. 예상 리스크

1. 리팩터링 도중 현재 live 안전장치를 약화시킬 수 있다.
2. legacy와 parity가 동시에 존재하면서 코드 중복이 잠시 늘어난다.
3. historical 데이터만으로 live fill을 완전히 재현할 수는 없다.

대응 원칙:

- 먼저 공통 타입과 pure logic을 추출하고
- broker I/O는 마지막에 감싼다

## 16. 최종 판단 기준

이 설계의 완료 기준은 "코드가 예쁘게 분리되었다"가 아니다.

완료 기준은 아래다.

1. 현재 `orb_fvg`에 대해 백테스트와 라이브의 차이를 구조적으로 설명할 수 있다.
2. 같은 날짜에 대해 `legacy`, `parity`, `replay`, `live`를 나란히 비교할 수 있다.
3. 새 전략을 도입해도 실행 규칙은 그대로 재사용할 수 있다.
4. 백테스트 결과를 다시 실거래 의사결정의 근거로 삼을 최소한의 구조가 갖춰진다.

## 17. 권장 결론

다음 개발 순서가 맞다.

1. 현재 백테스트를 `legacy baseline`으로 동결
2. `SignalEngine` 추출
3. `PositionManager` 추출
4. `ExecutionPolicy`를 현재 live 기준으로 명문화
5. `parity backtest` 작성
6. `replay` 작성
7. 그 후에야 `orb_vwap_pullback` 같은 새 전략 도입

즉, 지금 프로젝트의 최우선 과제는 전략 교체가 아니라  
**백테스트와 라이브가 같은 실행 언어를 사용하도록 만드는 것**이다.
