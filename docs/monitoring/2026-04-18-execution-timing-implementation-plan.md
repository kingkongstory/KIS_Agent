# 2026-04-18 실행 타이밍 구현 계획

## 문서 목적

이 문서는 [실행 타이밍 점검](2026-04-18-execution-timing-review.md)에서 확인한 문제를 실제 구현 작업으로 옮기기 위한 계획서다.

특히 이 문서는 단순 점검 항목 정리가 아니라, 아래 설계 의견을 **구현 계획 형태로 번역한 문서**다.

> 전략 로직은 최대한 유지하고, 실행 계층만 거의 다시 설계한다.
>
> 문제의 본질은 신호 품질보다 주문 상태와 실제 보유 상태가 늦게, 혹은 비대칭적으로 맞춰지는 것이다.
>
> 따라서 ORB+FVG를 먼저 바꾸지 않고, `live_runner`의 주문/체결/청산 회로를 우선 재설계한다.

이번 작업의 목적은 전략 수익률 개선이 아니다. 핵심 목적은 아래 하나다.

> **신호, 주문, 체결, 보유 상태, 청산 상태가 실제 계좌와 시스템 내부 상태에서 같은 순서와 같은 의미를 갖도록 강제하는 것**

구현 담당자가 가장 쉽게 오판하는 부분은 아래 두 가지다.

- 주문 응답 성공을 체결 성공으로 착각하는 것
- 청산 주문 제출을 flat 상태로 착각하는 것

이 문서는 그 오판을 막기 위해 **불변식**, **금지사항**, **상태 전이 규칙**, **수용 기준**을 명시한다.

## 이번 계획의 출발점

이번 계획은 아래 판단을 전제로 한다.

- 전략 철학은 우선 유지한다
- 먼저 바꿀 대상은 신호 생성이 아니라 실행 계층이다
- 실전 사고의 본질은 "신호가 틀렸다"보다 "상태 정합이 깨졌다"에 가깝다
- 따라서 구현 우선순위는 전략 개선이 아니라 실행 상태머신, 체결 재검증, 청산 대칭화다

이 문서를 읽는 구현 담당자는 이번 작업을 "전략 리뉴얼"로 이해하면 안 된다.
정확한 해석은 아래와 같다.

> **이번 작업은 실행 계층 재설계 작업이다.**

## 제안한 설계를 구현 계획으로 옮기면

내가 제안한 의견을 구현 계획 문장으로 옮기면 아래 8개 항목이다.

### 1. 전략 로직은 최대한 유지한다

- ORB+FVG 전략의 기본 철학은 이번 작업의 주대상이 아니다
- 우선은 실행 계층 정합성을 해결한다
- 전략 규칙 변경은 실행 계층 안정화 후에 검토한다

### 2. 실행 상태를 명시한다

내부 상태는 최소한 아래를 표현할 수 있어야 한다.

- `Flat`
- `EntryPending`
- `EntryPartial`
- `Open`
- `ExitPending`
- `Flat`

실제 구현에서는 `ManualIntervention`, `Degraded`, `SignalArmed`, `ExitPartial` 등이 추가될 수 있지만,
핵심은 아래 두 상태를 반드시 명시하는 것이다.

- `EntryPending`
- `ExitPending`

### 3. 청산에서 절대 먼저 flat 처리하지 않는다

청산 주문 제출과 flat 전이는 다른 사건이다.

- 청산 주문 제출
- 체결 확인
- 보유수량 0 확인
- 그 다음 flat 전이

이 순서를 강제해야 한다.

### 4. 청산도 entry 수준으로 재검증한다

시장가 청산 후에는 반드시 아래 순서가 필요하다.

1. WS 체결통보 확인
2. REST 체결조회 확인
3. 잔고 재확인
4. 보유수량이 남아 있으면 잔량 청산 또는 `manual_intervention`

즉, 청산은 진입보다 약한 회로가 아니라 **진입과 같은 수준의 검증 회로**가 되어야 한다.

### 5. 진입 정책은 패시브 지정가에서 시장성 지정가 계열로 바꾼다

현행 `PassiveTopBottomTimeout30s` 는 ETF 단기 매매 문맥에서 타이밍 손실이 크다.

따라서 실전 정책의 기본 방향은 아래다.

- 매수: `best ask + 1 tick`
- 매도: `best bid - 1 tick`
- 슬리피지 예산 부여
- timeout 단축
- 필요 시 1회까지만 추격 허용

### 6. 신호 감시는 5초 폴링에서 이벤트 기반으로 줄인다

기존 구조:

- 5초 폴링으로 진입 기회 탐색

목표 구조:

- 5분봉 확정 이벤트에서 신호 생성
- armed 상태에서는 틱/호가 기반으로 재진입 판단
- 최소한 100~300ms 타이머 또는 WS 가격 이벤트 기반 감시

### 7. 주문 응답 성공과 체결 완료를 같은 것으로 보지 않는다

강제 규칙:

- `EntryPending` 에서 체결 확인 전 `Open` 금지
- `ExitPending` 에서 보유수량 0 확인 전 `Flat` 금지
- WS/REST/balance 충돌 시 `manual_intervention` 또는 `degraded`

### 8. 복구와 분석을 위한 주문 상태 영속화를 추가한다

`active_positions` 만으로는 부족하다.

아래를 저장할 `orders` 또는 `execution_journal` 이 필요하다.

- `signal_id`
- `order_id`
- `state`
- `intended_price`
- `sent_at`
- `ack_at`
- `first_fill_at`
- `final_fill_at`
- `filled_qty`
- `holdings_before`
- `holdings_after`
- `resolution_source`

즉, 이 문서는 위 8개 설계 제안을 실제 구현 단계로 푸는 것이 목적이다.

## 이 문서가 해결하려는 질문

현재 시스템은 전략 신호가 발생해도, 실제로는 아래 회로가 정확히 맞아야만 정상 동작이라고 볼 수 있다.

1. 신호 발생
2. 주문 제출
3. 체결 확인
4. 포지션 상태 반영
5. 청산 주문 제출
6. 청산 체결 확인
7. 실제 보유 0 확인
8. flat 상태 반영

기존 점검 문서의 결론은 다음이었다.

- 진입 경로는 비교적 강하게 보강되어 있음
- 청산 경로는 진입만큼 대칭적이지 않음
- 신호 감시는 5초 폴링 중심이라 타이밍 손실 가능성이 있음
- 패시브 지정가 정책은 "적시에 체결"이라는 목표와 구조적으로 충돌 가능성이 있음
- 현재 진입 체결 대기 `30초`는 ETF 단기 신호 기준으로 과도하게 길 가능성이 높음

이 구현 계획은 위 4개를 실제 작업 항목으로 내리는 문서다.

## 범위

### 포함

- 실시간 실행 계층 구조 개편
- 진입/청산 상태 전이 명확화
- 청산 후 잔고 재검증 추가
- 종목별 직렬 실행 보장
- 체결 확인 경로 대칭화
- 실행 정책을 패시브 지정가에서 시장성 지정가 계열로 전환하는 기반 마련
- 신호 감시 타이밍 개선

### 제외

- ORB/FVG 전략 철학 자체 변경
- 지표 추가
- ETF 전용 필터 추가
- UI 디자인 개선

즉, 이번 작업은 **전략 로직 변경이 아니라 실행 안정성 재설계**다.

## ETF 규칙 초안과의 관계

이 문서는 [레버리지·인버스 ETF 전용 규칙 초안](../strategy/etf-leverage-inverse-rule-draft.md) 과 충돌하지 않아야 한다.

구현 담당자가 가장 쉽게 하는 실수는 아래 둘 중 하나다.

- ETF 규칙 문서의 전략 필터를 실행 계층 안에 하드코딩하는 것
- 반대로 ETF 문서에 적힌 실행 관련 요구사항을 "전략 문서니까 나중"으로 미루는 것

정확한 해석은 아래와 같다.

- 이 문서는 **실행 계층 재설계 문서**
- ETF 문서는 **전략/운영 규칙 문서**

둘은 역할이 다르지만, 일부 항목은 함께 구현되어야 한다.

## ETF 문서에서 이번 구현에 같이 반영해야 하는 항목

아래 항목은 ETF 문서에 적혀 있지만, 성격상 전략 로직이 아니라 실행/운영 계층과 맞닿아 있으므로 이번 구현 계획에 포함한다.

### 1. 실거래 진입 방식

ETF 문서에는 아래 문장이 있다.

- 실거래는 시장성 지정가 또는 공격적 지정가 검토

이 항목은 순수 전략 규칙이 아니라 실행 정책이다.
따라서 이번 구현 계획의 `시장성 지정가 정책 도입` 에 포함한다.

### 2. 오전 신규 진입 마감 시각

ETF 문서에는 아래 항목이 있다.

- 신규 진입 마감 기본값 `10:30`

이 값 자체는 전략 규칙이지만, **실행 엔진이 이해할 수 있는 cutoff 메커니즘**이 먼저 있어야 적용할 수 있다.
따라서 이번 구현에서는 최소한 아래를 지원해야 한다.

- 종목별/전략별 `entry_cutoff` 주입 가능
- armed 상태에서도 cutoff 지나면 즉시 진입 무효화
- pending 중 cutoff 도달 시 후속 처리 정책 명시

즉, `10:30`이라는 숫자를 지금 하드코딩하라는 뜻이 아니라,
ETF 문서의 cutoff 규칙을 나중에 안전하게 꽂을 수 있는 실행 구조를 먼저 만든다는 뜻이다.

### 3. 레버리지와 인버스를 동시에 보유하지 않음

ETF 문서에는 아래 운영 규칙이 있다.

- 레버리지와 인버스를 동시에 보유하지 않음
- 하루 한 방향만 거래

이 규칙은 전략 해석 문제이기도 하지만, 동시에 **실행 계층의 잠금 규칙**이기도 하다.
따라서 execution actor 로 개편하더라도 아래는 반드시 유지해야 한다.

- 종목 간 공용 lock 또는 그와 동등한 직렬화 규칙
- 다른 종목이 `Open` 또는 `ExitPending` 이면 신규 진입 차단
- 하루 한 방향 규칙을 강제할 수 있는 전역 gate 확장 여지

### 4. 실시간 구독 health가 비정상이면 신규 진입 금지

ETF 문서의 운영 규칙:

- 실시간체결/호가/NAV/지수 구독 중 하나라도 비정상이면 신규 진입 금지

이건 전략 규칙처럼 보이지만 실제로는 실행 가능성 판단이다.
따라서 이번 구현 계획의 `Degraded` 상태와 직접 연결해야 한다.

중요한 점은 아래다.

- health 신호는 Signal Engine 이 아니라 실행 엔진의 gate 로 적용
- health 비정상 시 기존 포지션 관리는 하되 신규 진입만 차단

### 5. 호가 스프레드 필터

ETF 문서에는 아래가 있다.

- 1호가 스프레드 제한
- 총매수/총매도 잔량 비율 확인

이 중 스프레드 제한은 실행 정책과 강하게 연결된다.
따라서 이번 계획에서는 아래를 준비해야 한다.

- 주문 직전 호가 스냅샷 주입
- 스프레드 체크 훅 제공
- 향후 `MarketableLimitWithBudget` 에서 preflight gate 로 사용 가능하게 설계

반면 잔량 비율은 전략/미시구조 해석에 더 가깝기 때문에 초기 실행 재설계 단계에서는 인터페이스만 고려하고 필수 구현 대상으로 두지 않는다.

## ETF 문서에서 지금 당장 구현 대상으로 오해하면 안 되는 항목

아래 항목은 중요하지만, 이번 실행 계층 재설계와 같은 단계에서 한꺼번에 넣으면 구현자가 책임을 섞을 위험이 크다.

### 1. 지수 레짐 판정

- 상승/하락/중립 레짐 판정
- 활성 ETF 선택

이건 Signal Engine 쪽 규칙이다.
이번 단계에서는 execution actor 가 이런 규칙을 직접 계산하면 안 된다.

필요한 것은 아래 정도다.

- `SignalIntent` 또는 그 메타데이터에 `active_etf`, `regime`, `regime_confirmed` 를 담을 수 있는 구조 준비

### 2. NAV 괴리 필터

NAV 괴리는 중요하지만, 구현 책임을 잘 나눠야 한다.

- 괴리율 계산: 전략/시세 집계 계층
- 진입 차단 적용: 실행 계층 preflight gate

즉, execution actor 가 직접 NAV를 계산하는 구조로 가면 안 된다.
지금 단계에서 필요한 것은 아래다.

- preflight rejection reason 에 `nav_gap_exceeded` 를 표현할 수 있는 구조

### 3. 프로그램매매, 거래량, FVG 최소폭

이 항목들은 모두 전략 필터다.
실행 계층 재설계 1차 구현에 섞으면 안 된다.

필요한 것은 단 하나다.

- Signal Engine 이 "이미 검증된 신호"를 Execution Engine 에 넘기는 인터페이스 정리

## 구현 담당자를 위한 경계선

아래 문장을 명확히 구분해야 한다.

### 실행 계층이 책임지는 것

- 주문 제출
- 체결 확인
- 잔고 검증
- 상태 전이
- timeout
- lock
- degraded/manual 처리
- 주문 상태 영속화
- cutoff, preflight gate 적용

### 전략 계층이 책임지는 것

- OR 계산
- FVG 탐지
- 레짐 판정
- NAV 괴리 계산
- 거래량/프로그램매매 해석
- 활성 ETF 선택
- 재진입 자격 판단

### 중요한 설계 원칙

전략 계층은 `진입 자격`을 결정하고,
실행 계층은 `자격이 있는 신호를 실제 주문과 체결로 완결`한다.

이 둘을 다시 섞으면 이번 재설계의 목적이 무너진다.

## 반드시 유지해야 할 해석

구현 중 아래 문장을 다르게 해석하면 안 된다.

### 1. 주문 접수는 체결이 아니다

`rt_cd == 0` 또는 주문번호 수신은 단지 **브로커가 주문을 받았다는 뜻**이다.

그 다음 확인이 따로 필요하다.

- WS 체결통보
- REST 체결조회
- 잔고 변화

### 2. 체결가를 모르는 것과 체결이 안 된 것은 다르다

시장가 청산 후 체결가 조회 실패는 다음 둘을 구분해야 한다.

- 체결은 됐지만 체결가를 아직 모름
- 체결 자체가 아직 불완전함

현재 구현은 이 구분이 약하다. 새 구현에서는 **체결가 미확인**과 **flat 미확인**을 절대 같은 것으로 취급하면 안 된다.

### 3. flat 상태는 보유수량 0 확인 이후에만 선언할 수 있다

아래는 금지한다.

- 청산 주문 보냄
- 체결가 못 찾음
- 현재가 fallback
- 내부 상태를 `None` 또는 `Flat` 으로 정리

이건 구현자가 가장 쉽게 저지르는 오판이다.

### 4. 전략 신호와 실행 타이밍은 분리해야 한다

신호는 "매수/매도 자격 발생"일 뿐이고, 실행은 "정해진 정책으로 주문/체결을 완결"하는 별도 계층이다.

둘을 같은 함수 안에서 섞으면 향후 유지보수 때 다시 깨진다.

## 핵심 불변식

이번 구현에서 지켜야 할 핵심 불변식은 아래와 같다.

### 불변식 1. `Open` 은 실보유 증가가 확인된 상태만 의미한다

아래 3개 중 최소 1개 이상으로 체결이 확인되어야 한다.

- WS 체결통보
- REST 체결조회
- balance 기준 보유수량 증가

확인 전에는 `EntryPending` 또는 `EntryPartial` 이다.

### 불변식 2. `Flat` 은 실보유 0이 확인된 상태만 의미한다

아래가 확인되기 전에는 절대 `Flat` 으로 전이하지 않는다.

- 보유수량 0

체결가를 모르면 `ExitPendingUnpriced`
보유가 남아 있으면 `ExitPartial`
상태 불명이면 `ManualIntervention`

이 불변식은 내가 제시한 의견 중 가장 중요한 문장을 그대로 구현 규칙으로 옮긴 것이다.

> 청산에서 절대 먼저 flat 처리하지 않는다.

특히 아래 예외를 두지 않는다.

- TP 지정가가 일부만 체결된 뒤 잔량을 시장가로 보냈다고 해서 바로 `Flat` 으로 전이하지 않는다
- 재청산 주문을 보냈다고 해서 `current_position` 을 제거하지 않는다
- 체결가를 몰라 현재가 fallback 을 썼다고 해서 `Flat` 이 성립하지 않는다

즉, **TP 부분 체결 경로도 일반 시장가 청산 경로와 같은 수준으로 `ExitPending -> verify -> Flat` 규칙을 따라야 한다.**

### 불변식 3. 종목당 실행 흐름은 하나만 존재한다

한 종목에 대해 아래가 동시에 돌면 안 된다.

- 신규 진입 실행
- 청산 실행
- 동일 종목 재진입 검토
- 재시작 복구 정합화

즉, 종목별로 **단일 execution actor** 가 필요하다.

### 불변식 4. pending 상태에서는 새로운 주문을 만들지 않는다

- `EntryPending` 중 추가 진입 금지
- `ExitPending` 중 신규 진입 금지
- `ExitPending` 중 추가 청산 주문은 잔량 처리 정책에 의해 명시적으로만 허용
- `SignalArmed` 중 같은 신호에 대한 중복 신규 주문 금지
- `EntryPartial` / `ExitPartial` 중 같은 종목의 다음 신호 스캔 금지

### 불변식 5. 상태 불명은 자동 정상화보다 manual intervention 이 우선이다

아래 상황에서는 자동 추정 복구를 금지한다.

- 체결은 일부 확인되는데 잔고와 수량이 일치하지 않음
- WS와 REST와 balance가 서로 충돌
- 청산 후 보유수량이 예상보다 큼
- 재시작 복구 시 주문/포지션 메타 부족

이때는 `manual_intervention_required` 로 가야 한다.

## 금지사항

아래 방식은 이번 구현에서 금지한다.

### 1. 청산 주문 직후 `current_position.take()` 후 flat 처리

청산 주문을 보내는 순간 내부 포지션을 제거하면, 이후 체결 실패/부분 체결/체결 지연에서 상태가 깨진다.

청산 시에는 `current_position` 을 지우는 대신 `ExitPending` 으로 바꾸는 구조가 필요하다.

### 2. 현재가 fallback 으로 flat 여부를 판단

현재가는 체결가 대체치일 수는 있어도 flat 여부 대체치가 아니다.

현재가 fallback은 아래 경우에만 제한적으로 허용한다.

- 이미 보유수량 0은 확인됨
- 단지 최종 체결가 기록만 누락됨

그 외에는 금지한다.

### 3. WS 미수신을 무체결로 해석

WS 체결통보가 없다고 해서 무체결로 간주하면 안 된다.
REST와 balance 확인이 뒤따라야 한다.

### 4. 실행 함수 안에서 바로 다음 신호 스캔을 열어두는 것

`EntryPending` 또는 `ExitPending` 중에는 같은 종목의 다음 신호 탐색을 잠그는 것이 맞다.

### 5. TP 부분 체결 후 잔량 주문 직후 flat 처리

이 경로는 구현자가 실수하기 가장 쉽다.

아래 순서는 금지한다.

1. TP 일부 체결 확인
2. 잔량 시장가 주문 제출
3. 내부 포지션 제거
4. `Flat` 전이

올바른 구조는 아래다.

1. TP 일부 체결 확인
2. `ExitPartial` 또는 `ExitPending` 전이
3. 잔량 청산 주문 제출
4. WS / REST / balance 재검증
5. holdings 0 확인 후에만 `Flat`

## 목표 상태 모델

이번 구현의 목표 상태 모델은 아래와 같다.

### 종목별 실행 상태

- `Flat`
- `SignalArmed`
- `EntryPending`
- `EntryPartial`
- `Open`
- `ExitPending`
- `ExitPartial`
- `ManualIntervention`
- `Degraded`

### 상태 의미

- `Flat`
  - 실보유 0 확인
- `SignalArmed`
  - 신호는 유효하나 아직 주문 안 나감
- `EntryPending`
  - 진입 주문 제출 완료, 체결 확인 중
- `EntryPartial`
  - 일부 체결, 잔여 처리 중
- `Open`
  - 포지션 보유 확인 완료
- `ExitPending`
  - 청산 주문 제출 완료, 청산 체결/잔고 확인 중
- `ExitPartial`
  - 일부 청산, 잔량 정리 중
- `ManualIntervention`
  - 자동 복구 금지, 수동 확인 필요
- `Degraded`
  - 시스템 신뢰도 부족으로 신규 진입 금지

## 목표 아키텍처

### 1. Signal Engine 과 Execution Engine 분리

Signal Engine 역할:

- OR 계산
- FVG 탐지
- 진입 후보 생성
- `SignalIntent` 생성

Execution Engine 역할:

- 주문 가격 결정
- 주문 제출
- 체결 확인
- 포지션 상태 전이
- 청산 실행
- 잔고 검증

현재 `live_runner.rs` 안에서 둘이 강하게 섞여 있으므로, 이번 작업에서는 최소한 개념적으로라도 분리한다.

### 2. 종목별 Execution Actor 도입

각 종목마다 하나의 actor가 아래 이벤트만 처리한다.

- `SignalTriggered`
- `EntryOrderAccepted`
- `ExecutionNoticeReceived`
- `ExecutionQueryResolved`
- `BalanceReconciled`
- `ExitRequested`
- `ExitOrderAccepted`
- `TimeoutExpired`
- `ManualInterventionRequired`

이 actor만 해당 종목의 상태를 바꿀 수 있도록 한다.

여기서 중요한 점은 timeout도 actor 책임이어야 한다는 것이다.

- 진입 timeout
- 청산 확인 timeout
- 부분 체결 후 잔량 처리 timeout

이 값들을 각 함수에 흩뿌리면 구현자가 `30초` 같은 숫자를 다른 의미로 재사용할 위험이 커진다.

### 3. Execution Journal 영속화

현재 `active_positions` 만으로는 부족하다.

다음 중 하나를 추가한다.

- `execution_journal` 테이블
- 또는 `order_journal` 테이블

최소 컬럼:

- `stock_code`
- `signal_id`
- `execution_id`
- `phase`
- `broker_order_id`
- `intended_price`
- `submitted_price`
- `filled_price`
- `filled_qty`
- `holdings_before`
- `holdings_after`
- `resolution_source`
- `created_at`
- `updated_at`

이 테이블은 재시작 복구와 사후 분석의 핵심이다.

실제 구현에서는 아래 시각 컬럼 또는 동등한 metadata 키가 추가되어야 한다.

- `sent_at`
- `ack_at`
- `first_fill_at`
- `final_fill_at`

중요한 점은 이 테이블이 "최종 체결 결과만 저장하는 테이블"이 아니라는 것이다.
최소한 아래 phase 를 남길 수 있어야 재시작 복구와 사후 분석이 된다.

- `entry_submitted`
- `entry_acknowledged`
- `entry_first_fill`
- `entry_final_fill`
- `entry_cancelled`
- `exit_submitted`
- `exit_first_fill`
- `exit_final_fill`
- `exit_retry_submitted`
- `exit_retry_final_fill`
- `manual_intervention`

즉, `active_positions` 는 "현재 보유 snapshot" 이고, `execution_journal` 은 "주문 라이프사이클 event log" 다.
둘은 대체 관계가 아니라 보완 관계다.

## 구현 단계

구현 단계는 한 번에 크게 뜯지 말고, 아래 순서로 나눈다.

이 순서는 내가 제시했던 우선순위를 구현 단계로 변환한 것이다.

## Phase 1. 청산 경로 대칭화

### 목표

기존 전략 로직은 최대한 유지하되, 가장 위험한 청산 경로를 진입 수준으로 보강한다.

이 단계는 내가 제시했던 아래 의견을 직접 구현하는 단계다.

> 내가 가장 먼저 바꿀 것은 청산 경로다.
>
> 청산 후에는 WS 확인, REST 확인, 잔고 재확인, 잔량 처리 또는 manual_intervention 까지 가야 한다.

### 핵심 작업

1. `close_position_market()` 를 `즉시 flat 처리` 방식에서 `ExitPending` 방식으로 변경
2. 시장가 청산 후 아래 순서로 확인
   - WS 체결통보
   - REST 체결조회
   - balance 재검증
3. 청산 후 holdings가 0이 아닐 경우
   - `ExitPartial` 또는 `ManualIntervention`
4. 체결가 미확인이라도 holdings 0이면
   - flat 가능
   - 단, fill price unknown 상태로 기록
5. `current_position` 제거 시점은 holdings 0 확인 이후로 이동
6. TP 부분 체결 경로도 동일 원칙 적용
   - TP 일부 체결 후 잔량 시장가 주문을 보내더라도 즉시 `Flat` 전이 금지
   - 잔량 청산 역시 `WS -> REST -> balance` 재검증 수행
7. 청산 재시도 경로의 원주문번호와 재주문번호를 모두 journal 에 남김
8. `exit_verification_budget` 을 실제 세부 timeout 으로 분배
   - WS 대기
   - REST 폴링
   - balance 재확인
   - 잔량 재청산 후 재검증

### 수정 대상

- `src/strategy/live_runner.rs`
- `src/infrastructure/cache/postgres_store.rs`

### 이번 단계에서 꼭 들어가야 하는 review 문서 후속 항목

아래는 [실행 타이밍 점검](2026-04-18-execution-timing-review.md) 에서 이미 구현 필요로 명시한 항목이며, 반드시 Phase 1 에 포함한다.

- 시장가 청산 후 잔고 재검증 추가
- 청산도 entry 수준의 WS + REST + balance 확인 구조 적용
- 청산 부분 체결 시 후속 정책 추가
- TP 부분 체결 후 잔량 청산도 동일 verify 회로 적용
- "현재가 fallback 으로 종료"는 holdings 0 확인 이후의 price_unknown 기록에만 제한

### 완료 기준

- 청산 주문 제출만으로 flat 처리되지 않음
- 청산 후 holdings 0 확인 전까지 `ExitPending` 유지
- 청산 후 보유가 남으면 자동 flat 금지
- WS 미수신/REST 지연 상황에서도 balance로 최종 판정 가능
- TP 부분 체결 후 잔량 청산 경로에서도 holdings 0 확인 전 `Flat` 금지
- 재청산 주문 경로의 원주문번호/재주문번호 상관관계가 journal 에 남음

## Phase 2. 실행 상태 머신 명시화

### 목표

현재 `current_position: Option<Position>` 중심 구조를 실행 상태 중심 구조로 바꾼다.

이 단계는 내가 제시했던 아래 의견을 구현하는 단계다.

> 내부 상태를 `Flat -> EntryPending -> EntryPartial -> Open -> ExitPending -> Flat` 으로 명시한다.

### 핵심 작업

1. `RunnerState` 또는 별도 구조체에 실행 상태 추가
2. `EntryPending`, `ExitPending`, `ManualIntervention` 상태를 명시화
3. 상태 전이 함수를 한 곳으로 모음
4. 같은 종목에서 상태 변경 권한을 한 경로로 제한
5. Signal Engine 이 넘긴 preflight metadata 를 보존할 구조 준비
6. `EntryPartial`, `ExitPartial`, `Degraded` 를 실제 운용 상태로 사용
7. 재시작 시 `EntryPending` / `ExitPending` / `ManualIntervention` 복구 정책 명시
8. legacy bool (`current_position`, `exit_pending`, `manual_intervention_required`) 와
   새 상태머신 간 ownership 을 한 방향으로 정리

예:

- `entry_cutoff`
- `regime_confirmed`
- `spread_ok`
- `nav_gate_ok`
- `active_instrument`

주의:

- 위 필드는 최종 구현에서 placeholder 기본값으로 남아 있으면 안 된다
- `entry_cutoff`, `spread_ok`, `active_instrument`, `subscription_health_ok` 는 최소한 실데이터 기반으로 채워져야 한다
- `nav_gate_ok`, `regime_confirmed` 는 전략/데이터 계층이 계산하고 실행 계층은 gate 적용만 한다

여기서 구현자가 오판하지 않도록 아래를 명시한다.

- `EntryPartial`
  - 일부 체결 + 잔량 취소/확인/정리 중
  - `filled_qty > 0` 만으로 바로 `Open` 금지
  - holdings 증가 또는 최종 체결 확정이 뒤따라야 `Open`
- `Degraded`
  - 데이터 신뢰도 부족으로 신규 진입만 차단
  - 기존 포지션 관리/청산은 유지
  - `ManualIntervention` 과 같은 상태가 아니다
- 재시작 복구
  - `Open` 만 복구하는 것으로 끝나면 안 된다
  - 최소한 `ExitPending` 상태는 복구되어야 한다
  - 가능하면 `EntryPending` 도 journal 기반으로 복구 또는 manual 분기 정책이 필요하다

### 수정 대상

- `src/strategy/live_runner.rs`
- 필요 시 `src/strategy/execution_actor.rs` 신규 추가

### 완료 기준

- 진입/청산 중간 상태가 명시적으로 표현됨
- 상태 전이 로그가 일관됨
- 상태 전이 중복 호출에 대해 idempotent 하게 동작
- `EntryPartial` / `ExitPartial` / `Degraded` 가 enum 선언만이 아니라 실제 런타임 전이로 사용됨
- 재시작 시 pending 상태가 유실되지 않음

## Phase 3. 시장성 지정가 정책 도입

### 목표

적시 체결이 필요한 전략에 맞춰 실행 정책을 패시브 지정가에서 시장성 지정가 계열로 이동한다.

이 단계는 내가 제시했던 아래 의견을 직접 구현하는 단계다.

> 실전 정책은 시장성 지정가로 간다.
>
> 매수는 best ask + 1 tick, 매도는 best bid - 1 tick 같은 방식으로 체결 확률을 우선한다.

### 핵심 작업

1. `ExecutionPolicy` 에 신규 정책 추가
   - 예: `MarketableLimitWithBudget`
2. 진입 가격 정책 정의
   - 매수: `best ask + N tick`
   - 매도: `best bid - N tick`
3. timeout 단축
   - 예: 2~3초
4. 슬리피지 예산 추가
   - tick 기준 또는 pct 기준
5. timeout 후 후속 정책 명시
   - 추격 1회 허용 또는
   - 동일 신호 재진입 금지
   - 둘 중 하나를 설정으로 고정

### `30초`에 대한 명시적 판단

현행 `execute_entry()` 의 체결 대기 `30초`는 그대로 유지하면 안 된다.

이 문서의 권장 해석은 아래와 같다.

- `30초`는 안정성 값이 아니라 **전략 부적합 값**
- ETF ORB/FVG 단기 진입에서는 체결 대기가 길수록 신호 품질이 빠르게 열화
- 따라서 진입 timeout 은 **기본 2~3초**, 길어도 **5초 이하**가 맞다

즉, 구현 담당자는 `30초를 환경변수화해서 남겨두자`가 기본안이라고 생각하면 안 된다.
기본 방향은 **단축**이다.

### 주의

이 단계는 체결률을 올리지만, 잘못 구현하면 단순 추격매매가 된다.
반드시 아래 2개를 같이 넣는다.

- 슬리피지 한도
- timeout 후 재발주 제한

그리고 timeout 은 아래처럼 분리해야 한다.

- `entry_submit_timeout`
  - 주문 제출 응답 timeout
- `entry_fill_timeout`
  - 진입 체결 대기 timeout
- `exit_fill_timeout`
  - 청산 체결/보유 확인 timeout

현재처럼 "하나의 30초"로 뭉뚱그리면 구현자가 제출 timeout, 체결 timeout, 재검증 timeout 을 혼동할 수 있다.

또한 이 단계에서는 아래를 같이 확정해야 한다.

- 시장성 지정가 anchor 는 `best ask` / `best bid` 우선
- 호가가 stale 이면 last trade fallback
- fallback 사용 여부는 metadata / journal 에 남긴다
- slippage budget 초과 시 "체결률을 위해 강행" 하지 않고 진입 포기

### 수정 대상

- `src/strategy/parity/execution_policy.rs`
- `src/strategy/live_runner.rs`

### ETF 문서와의 직접 연결 포인트

이 단계는 ETF 문서의 아래 항목을 실제 코드로 받쳐주는 단계다.

- 실거래는 시장성 지정가 또는 공격적 지정가
- 스프레드 제한
- 오전 진입 마감 시각

즉, ETF 문서의 실행 관련 요구사항은 이 단계에서 흡수한다.

### 완료 기준

- 현행 패시브 지정가와 새 시장성 지정가 정책을 설정으로 전환 가능
- 정책별 체결률/슬리피지 비교 가능
- timeout 후 1회 추격 또는 재진입 금지 정책이 구현상 명시됨

## Phase 4. 신호 감시 타이밍 개선

### 목표

5초 폴링 기반 진입 감시를 줄이고, 신호를 더 적시에 실행한다.

이 단계는 내가 제시했던 아래 의견을 구현하는 단계다.

> 5분봉 확정 이벤트에서 신호를 만들고, armed 상태에서는 틱/호가 이벤트 기반으로 진입을 본다.

### 핵심 작업

1. 5분봉 확정 이벤트 기반으로 SignalIntent 생성
2. `SignalArmed` 상태 도입
3. FVG 재진입 감시는 틱/호가 이벤트 기반 또는 더 짧은 주기로 감시
4. `poll_and_enter()` 와 가격 리스너 역할 재분리
5. armed 상태에서의 감시 주기 명시
   - 목표: WS 가격 이벤트 또는 100~300ms 타이머
   - 최소 기준: 1초보다 더 짧은 감시로 전환 가능한 구조
6. armed 상태 무효화 조건 명시
   - cutoff 도달
   - active instrument 변경
   - NAV gate 실패
   - spread 급확대
   - subscription health 비정상

### 주의

이 단계에서 구현자가 자주 하는 실수는 "틱마다 전략 전체를 다시 계산"하는 것이다.
그렇게 하지 않는다.

올바른 구조는 아래다.

- 전략 계산: 5분봉 확정 시
- 진입 트리거 감시: armed 상태일 때만 실시간

또한 구현자는 아래를 혼동하면 안 된다.

- "5분봉 확정 이벤트에서 신호 생성" 과
- "armed 상태에서 실시간 진입 감시"

이 둘은 다른 단계다.
첫 번째는 전략 계산이고, 두 번째는 실행 타이밍 문제다.

### 완료 기준

- 전체 전략 재계산은 여전히 바 이벤트 중심
- armed 상태에서만 고빈도 감시
- 5초 폴링 의존도 감소
- armed 상태에서 100~300ms 또는 WS 이벤트 기반 감시 경로가 존재
- cutoff / health / spread / NAV / active instrument 변화가 armed 취소로 반영

### ETF 문서와의 직접 연결 포인트

이 단계는 ETF 문서의 아래 항목을 구현 가능하게 만든다.

- FVG 재진입 시 첫 진입만 유효
- 오전 시간대 집중
- 장중 호가/NAV/레짐 상태가 바뀌면 armed 신호 무효화

## 코드 레벨 권장 변경

내가 제시했던 코드 단위 분리는 아래와 같다.

- `src/strategy/live_runner.rs`
  - 신호 탐색과 실행 분리
- `src/strategy/execution_actor.rs`
  - 신규 추가, 주문/체결/청산 상태 머신 담당
- `src/strategy/parity/execution_policy.rs`
  - `MarketableLimitWithBudget` 추가
- `src/infrastructure/cache/postgres_store.rs`
  - 주문 상태 영속화 추가
- WebSocket 체결통보 파싱
  - 주문번호 기준 correlation 강화

아래 세부 변경은 위 코드 분리를 구현하는 과정의 세부 항목이다.

## 1. `close_position_market()` 재설계

현재 함수는 "청산 주문 제출 + 체결가 조회" 중심이다.
새 버전은 아래 책임을 가져야 한다.

- `ExitPending` 전이
- order id 저장
- exit verification loop 실행
- holdings 0 확인 후에만 flat 전이
- 실패 시 manual_intervention

즉, 함수 이름은 유지하더라도 내부 의미는 `submit_exit_and_resolve()` 에 가까워져야 한다.

## 1-1. `execute_entry()` timeout 분해

현재의 단일 `fill_timeout = 30초`는 아래처럼 분해해야 한다.

- 주문 응답 timeout
- 체결 대기 timeout
- 취소 후 재검증 timeout
- balance 재검증 timeout

권장 기본값:

- 주문 응답: `1~2초`
- 진입 체결 대기: `2~3초`
- 취소 후 재검증: `1~2초`
- balance 재검증: `3~5초`

중요한 점은 `30초 대기 후 취소` 구조를 유지하는 것이 아니라,
**짧게 확인하고 빨리 실패를 확정하는 방향**으로 바꾸는 것이다.

## 2. `fetch_fill_detail()` 역할 분리

현재는 체결가 조회 함수 성격이 강하다.
이후 구조에서는 아래 둘을 분리하는 것이 좋다.

- 체결 수량/체결가 확인
- holdings reconciliation

체결 정보와 보유 상태 확인은 구분해야 한다.

여기에 추가로 아래를 명시한다.

- TP 지정가 일부 체결 경로에서도 `fetch_fill_detail()` 결과만으로 종료하지 않는다
- 잔량 주문이 발생한 순간부터는 일반 청산 verify 회로로 이관한다
- 즉, "TP 확인 함수" 와 "청산 상태 해소 함수" 는 책임이 다르다

## 3. `current_position` 모델 재검토

지금처럼 `Option<Position>` 하나로는 아래를 표현하기 어렵다.

- 청산 중
- 부분 체결 중
- 체결가 미확인
- holdings mismatch

따라서 아래 둘 중 하나가 필요하다.

- `Position + ExecutionState` 조합
- `ExecutionSlot` 구조체로 통합

그리고 아래 요구사항을 충족해야 한다.

- `ExecutionSlot` 또는 동등 구조가 broker order id, signal id, filled qty,
  holdings_before, holdings_after, last_resolution_source 를 함께 들고 있어야 한다
- pending 상태 복구 시 `active_positions` 와 `execution_journal` 를 함께 사용할 수 있어야 한다

## 3-1. 재시작 복구 범위 확장

이 문서의 원안에는 "재시작 시 `ExitPending` 상태 복구"가 포함돼 있었다.
구현 단계에서 이 항목이 빠지지 않도록 별도로 못 박는다.

최소 요구사항:

- `Open` 상태만 복구해서 끝내지 않는다
- `ExitPending` 은 재시작 후에도 "청산 중" 상태로 이어지거나 `manual_intervention` 으로 승격되어야 한다
- `EntryPending` 은 journal 과 balance 기준으로
  - 체결 확인 가능하면 `Open`
  - 미체결 확정 가능하면 `Flat`
  - 불명확하면 `ManualIntervention`
- pending 상태 복구 시 원주문번호, 최근 phase, holdings snapshot 을 함께 본다

구현자가 가장 쉽게 하는 오판은 "서버 재시작 시 pending 은 그냥 버린다"인데, 이건 금지한다.
pending 을 버리면 실계좌 상태와 내부 상태의 시간축이 다시 어긋난다.

## 테스트 계획

이번 작업은 단위 테스트보다 **상태 전이 테스트**가 더 중요하다.

## 필수 시나리오

1. 진입 주문 접수 후 WS 체결통보 수신
2. 진입 주문 접수 후 WS 미수신, REST 체결 확인
3. 진입 취소 후 balance 증가 감지
4. 시장가 청산 후 WS 체결통보 수신
5. 시장가 청산 후 WS 미수신, REST 체결 확인
6. 시장가 청산 후 체결가 미확인 but holdings 0
7. 시장가 청산 후 holdings 잔존
8. TP 부분 체결 후 잔량 정리
9. 재시작 시 `ExitPending` 상태 복구
10. WS/REST/balance 충돌 시 manual_intervention 전이
11. `entry_fill_timeout=2~3초` 내 미체결 시 정상 취소 + 상태 정리
12. 과거 `30초` 대비 짧은 timeout 으로도 동일 종목 반복 발주가 발생하지 않음
13. TP 부분 체결 후 잔량 주문을 보낸 직후 `Flat` 으로 전이하지 않음
14. `Degraded` 상태에서 신규 진입만 차단되고 기존 포지션 관리는 계속됨
15. `EntryPartial` 상태에서 holdings 증가 확인 전 `Open` 으로 가지 않음
16. `execution_journal` 에 `submitted -> ack -> first_fill -> final_fill` 또는 동등 metadata 가 남음

## 반드시 확인할 문장

테스트가 아래 문장을 증명해야 한다.

> 주문 응답 성공과 포지션 상태 전이는 같은 사건이 아니다.

> 청산 주문 제출과 flat 상태 전이는 같은 사건이 아니다.

## 구현 담당자 체크리스트

구현 전에 아래를 읽고 시작한다.

- [실행 타이밍 점검](2026-04-18-execution-timing-review.md)
- 이 문서의 `핵심 불변식`
- 이 문서의 `금지사항`

구현 중 아래를 계속 확인한다.

- 지금 바꾸는 코드는 체결가 문제인가, 체결수량 문제인가, holdings 문제인가
- 지금 상태 전이는 브로커 응답 기준인가, 실제 보유 기준인가
- 지금 flat 처리 조건이 보유수량 0 확인 이후인가
- 같은 종목에서 동시 실행 경로가 열려 있지는 않은가
- 지금 넣으려는 규칙이 실행 계층 책임인지, ETF 전략 계층 책임인지 구분했는가
- ETF 문서의 항목을 구현할 때 계산과 gate 적용 위치를 분리했는가

또한 구현 담당자는 아래 질문에 `예`라고 답할 수 있어야 한다.

- 이번 작업의 1순위가 전략 개선이 아니라 실행 계층 재설계라는 점을 이해했는가
- `Flat` 은 체결가 확인이 아니라 holdings 0 확인 기준이라는 점을 이해했는가
- `30초` timeout 을 유지하는 것이 아니라 줄이는 방향이 기본이라는 점을 이해했는가
- `live_runner` 를 줄이고 별도 execution actor 로 책임을 나누는 방향이라는 점을 이해했는가
- ETF 문서의 지수/NAV/거래량 규칙을 execution actor 안에서 직접 계산하면 안 된다는 점을 이해했는가

## 원안 대조 체크리스트

아래 항목은 내가 처음 제시했던 방향과 이 문서가 1:1로 대응하는지 확인하기 위한 체크리스트다.
구현 담당자는 작업 시작 전에 빠진 줄이 없는지 이 목록으로 대조한다.

### 전략 유지와 실행 계층 우선

- 전략 로직은 최대한 유지하고 실행 계층을 우선 재설계한다
- ORB+FVG 자체보다 주문/체결/청산 회로를 먼저 고친다

### 상태머신

- `Flat -> EntryPending -> EntryPartial -> Open -> ExitPending -> Flat`
- 청산에서는 holdings 0 확인 전 `Flat` 금지
- `EntryPending` 중 신규 진입 금지
- `ExitPending` 중 반대 주문 추가 금지
- WS/REST/balance 충돌 시 `Degraded` 또는 `ManualIntervention`

### 청산 재검증

- 청산 후 확인 순서: WS -> REST -> balance -> 잔량 처리 또는 manual
- TP 부분 체결 후 잔량 청산도 같은 검증 회로 사용
- 현재가 fallback 은 holdings 0 확인 후 `fill_price_unknown` 기록에만 사용

### 진입 정책

- 실전 기본은 시장성 지정가 계열
- `best ask + 1 tick` / `best bid - 1 tick` 계열 anchor 사용
- 슬리피지 예산 사용
- timeout 은 기본 2~3초, 길어도 5초 이하
- timeout 후 1회 추격 또는 재진입 금지 정책 명시

### 신호 감시

- 5분봉 확정 이벤트에서 신호 생성
- armed 상태에서는 틱/호가 또는 100~300ms 감시
- 손절 감지 속도와 진입 감지 속도의 비대칭을 줄인다

### DB / 복구 / 상관관계

- `active_positions` 만으로 끝내지 않는다
- `execution_journal` 또는 동등 구조에 주문 라이프사이클을 남긴다
- `signal_id`, `order_id`, `state`, `intended_price`, `sent_at`, `ack_at`,
  `first_fill_at`, `final_fill_at`, `filled_qty`, `holdings_before`, `holdings_after`,
  `resolution_source` 가 보존된다
- 재시작 시 `Open` 뿐 아니라 `ExitPending` / `EntryPending` 도 복구 또는 manual 분기 정책이 있다

### 코드 구조

- `live_runner.rs` 는 신호 탐색과 실행을 분리하는 방향으로 줄인다
- `execution_actor.rs` 또는 동등 구조가 주문/체결/청산 상태머신을 담당한다
- `execution_policy.rs` 가 시장성 지정가 정책을 표현한다
- WS 체결통보는 주문번호 기준 correlation 이 가능해야 한다

## 내가 제시했던 우선순위를 구현 순서로 다시 쓰면

아래 순서는 내가 처음 제시했던 우선순위를 구현 계획 문장으로 다시 쓴 것이다.

1. 청산 경로를 `ExitPending + 잔고 재검증`으로 고친다
2. 진입 정책을 패시브 지정가에서 시장성 지정가로 바꾼다
3. 진입 신호 감시를 5초 폴링에서 이벤트 기반으로 줄인다
4. 종목별 execution actor와 execution journal을 도입한다

즉, 전략 엔진보다 실행 엔진이 먼저다.

## 최종 완료 기준

아래 5개가 모두 만족되어야 이번 작업이 끝난 것으로 본다.

1. 청산 경로가 진입 경로 수준으로 재검증된다
2. flat 상태는 holdings 0 확인 후에만 전이된다
3. 상태 불명 시 자동 추정 대신 manual_intervention 으로 전이한다
4. 종목별 실행 흐름이 직렬화된다
5. 신호 감시와 실행 처리의 책임이 코드 구조상 분리된다

## 요약

이번 구현의 핵심은 "더 빨리 주문하는 것"이 아니다.
핵심은 아래 두 문장을 시스템에 강제하는 것이다.

> 주문이 나갔다고 포지션이 열린 것이 아니다.

> 청산 주문이 나갔다고 포지션이 닫힌 것이 아니다.

이 두 문장을 코드 구조와 상태 전이로 강제하지 않으면,
전략을 아무리 다듬어도 실전 운용에서는 다시 같은 류의 사고가 반복된다.
