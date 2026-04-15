# KIS 자동매매 전략 재설계 및 실거래 안정화 계획

## Context

2026-04-14에는 FVG 반복 발주로 거래 0건, 2026-04-15에는 cancel/fill race condition으로 실제 KIS 보유와 시스템 상태가 분리되며 -225,703원(-2.27%) 손실이 발생했다.

문제는 두 층위다.

1. **실거래 실행 안정성 결함**
   - cancel 응답을 사실상 "미체결 확정"처럼 취급
   - `filled_qty == 0` 이후 `inquire-balance` 재검증 부재
   - WS 체결통보가 없거나 불완전할 때도 거래 계속 진행
   - 재시작 복구 시 원전략 메타가 없는데도 임의 리스크로 포지션을 재구성
2. **전략/체결 모델 불일치**
   - 전략은 장초 ORB 철학인데 실제 구현은 Multi-Stage 5m/15m/30m로 약한 신호 허용
   - 백테스트와 라이브의 진입가/체결 규칙이 다름
   - 패시브 지정가를 실거래에 쓰면서 백테스트는 "터치 시 체결"에 가깝게 계산
   - 매수 타이밍이 늦고, 되돌림 확인 없이 zone 터치만으로 진입 시도

## 우선순위

### P0 — 2026-04-16 실거래 오류 방지

2026-04-16 KST 장에서는 **전략 개선보다 오류 없는 동작**이 우선이다.

- orphan position 0건
- cancel 후 실제 보유 발생 시 반드시 포지션 복구
- WS 체결통보 비정상 상태에서는 거래 시작 금지 또는 degraded 모드 고정
- `auto_start` 기본 OFF
- 하루 최대 1종목, 1회 진입

### P1 — 새 전략 도입

P0 완료 후 새 전략 `orb_vwap_pullback`을 도입한다.

### P2 — 백테스트/라이브 정합

전략 규칙과 체결 규칙을 분리하고, 공통 `ExecutionPolicy`로 백테스트와 라이브를 가능한 한 같은 규칙으로 맞춘다.

## 핵심 설계 원칙

1. **전략 로직과 주문 실행 로직을 분리한다.**
2. **백테스트는 신호 재현이 아니라 실행 포함 기대값 재현을 목표로 한다.**
3. **실거래는 패시브 지정가 중심이 아니라, 제한된 슬리피지 안의 시장성 지정가(marketable limit)를 기본으로 한다.**
4. **복구 불완전 시 임의 포지션 재구성 금지. 사람이 개입해야 하는 상태를 명시한다.**

## 새 전략 개요: `orb_vwap_pullback`

### 1. OR 15분 단일화

- OR은 `09:00~09:14` 1분봉 기준 `15분 OR` 하나만 사용
- 기존 5m/15m/30m Multi-Stage는 진입 판단에서 제거
- UI에는 `15m OR high/low`만 표시

### 2. VWAP 기반 방향 확인

- `09:15` 이후 5분봉 2개가 같은 방향으로 OR 밖에서 마감
- 동시에 VWAP도 같은 방향 정합성을 보여야 함
- 이 조건이 깨지면 그날 해당 방향 진입 중단

### 3. 첫 되돌림만 감시

- 방향 확정 후 `entry zone = OR 경계와 VWAP 사이 구간`
- 가격이 zone에 처음 들어오면 즉시 발주하지 않고 `armed` 상태만 설정
- 그 뒤 1분봉 반전 확인이 나오면 진입

### 4. 1분 반전 확인 후 진입

롱 기준:

- 가격이 위에서 내려와 zone에 진입
- 이후 1분봉 종가가 VWAP 위로 회복
- 필요하면 직전 1분봉 고가 돌파를 추가 조건으로 사용

이 규칙을 둬야 "zone 터치 즉시 패시브 지정가"보다 실거래와 백테스트를 더 가깝게 맞출 수 있다.

### 5. 종목 선택

- 상승 방향 확정 시 `122630`만 Long
- 하락 방향 확정 시 `114800`만 Long
- 하루 최대 진입은 우선 `1회`

### 6. 청산 단순화

- `1R`에서 절반 청산
- 나머지는 본절 이동
- 이후 `직전 5분봉 저점/고점` 또는 `VWAP 이탈` 기준으로 추적
- 신규 진입 cutoff `10:30`
- 강제 청산 `15:15`

## 체결 모델 수정

### 현재 문제

현재는 패시브 지정가로 진입하려고 하면서, 백테스트는 zone 터치만으로 체결에 가까운 가정을 둔다. 이 구조는 실거래와 일치하기 어렵다.

### 수정 원칙

- 패시브 지정가를 기본 진입 방식으로 두지 않는다.
- 신호가 나온 뒤, **1분 반전 확인 시점**에 **시장성 지정가**를 낸다.
- 예: 롱 진입 시 `limit = best ask + 1 tick`
- 단, `max_slippage_ticks` 또는 `max_entry_drift_pct`를 넘으면 주문하지 않는다.
- 주문 후 `2~3초` 내 미체결이면 즉시 취소하고 재진입하지 않는다.

### 백테스트 정합

새 전략에서는 `mid_price`나 단순 `touch fill`을 쓰지 않는다.

- 백테스트 진입가 = `반전 확인 시점 다음 체결 가능한 가격`
- 실거래 진입가 = `시장성 지정가 체결가`
- 둘 다 동일 `ExecutionPolicy`에서 계산

즉, 목표는 "완전 동일"이 아니라 **동일한 trigger / 동일한 slippage budget / 동일한 timeout 규칙**이다.

## P0 상세 — 2026-04-16 장 전까지

### 1. 주문 상태 진실값 재정의

우선순위:

1. WS 체결통보
2. `inquire-daily-ccld`
3. `inquire-balance`

단, **취소 후 `filled_qty == 0`이면 반드시 balance 재확인**한다.

### 2. WS 체결통보를 선택사항으로 두지 않음

다음 조건 중 하나라도 만족하면 실거래 시작 금지 또는 `degraded` 상태로 고정:

- `KIS_HTS_ID` 미설정
- 체결통보 구독 실패
- AES key/iv 미수립
- 장 시작 전 health check에서 체결통보 경로 비정상

### 3. 재시작 복구 보수화

현재처럼 원전략 메타 없이 `0.3%` 또는 다른 임의 리스크로 포지션을 재구성하지 않는다.

- active position 메타가 충분하면 동일 포지션 복구
- 메타가 부족하면 `manual_intervention_required` 이벤트 기록
- 해당 러너 자동매매 중단

### 4. 자동 시작 기본 OFF

- `auto_start` 기본값 OFF
- 2026-04-16은 수동 시작만 허용
- 1종목만 수동 시작

### 5. 진입 횟수 제한

- 하루 최대 1회 진입
- 종목당이 아니라 시스템 전체 기준 1회로 시작해도 됨

## P1 상세 — 새 전략 구현

### 수정 파일

| 파일 | 변경 |
|---|---|
| `src/strategy/orb_vwap_pullback.rs` | 새 전략 모듈 추가 |
| `src/strategy/mod.rs` | 모듈 export 추가 |
| `src/strategy/live_runner.rs` | 새 전략 연결, armed 상태, 1분 반전 확인, 시장성 지정가 진입 |
| `src/strategy/backtest.rs` | 새 전략용 backtest + execution policy 연결 |
| `src/domain/indicators/volume.rs` | 기존 VWAP 재사용 |
| `src/presentation/routes/strategy.rs` | 상태 노출 (`direction`, `armed`, `entry_zone`, `degraded`) |
| `frontend/src/components/strategy/*` | 전략 상태 패널 최소 확장 |

### 구현 순서

1. OR 15분 단일화
2. VWAP 계산 연결
3. 방향 확인 상태 머신
4. `armed` 상태 도입
5. 1분 반전 확인
6. 시장성 지정가 진입
7. 단순 청산

## P2 상세 — 백테스트/라이브 정합

### 목표

기존 `orb_fvg`는 유지하되, 새 전략은 공통 실행 규칙을 사용한다.

### 공통화 대상

- trigger 생성 규칙
- 주문 유효시간 (`2~3초`)
- 최대 허용 슬리피지
- 부분 체결 처리 정책
- 진입 실패 후 재진입 금지 규칙

### 구현 방식

- `SignalEngine`: OR/VWAP/direction/armed/entry trigger 생성
- `ExecutionPolicy`: 시장성 지정가 가격, timeout, 슬리피지 한계 계산
- `ExecutionSimulator`: 백테스트용 체결 시뮬레이션
- `LiveRunner`: 동일 `ExecutionPolicy` 기반 실주문

같은 규칙을 백테스트와 라이브에서 따로 다시 구현하지 않는다.

## 4개 PR 계획

### PR1 — 실거래 안정화 핫픽스 (P0)

**목표**: 2026-04-16 KST 장에서 오류 없는 동작 보장

- `cancel_all_pending_orders` 반환형을 `Result<usize, KisError>`로 변경
- `execute_entry` 미체결 분기에 balance 재검증 추가
- 진입 전 `holding_qty_before`, 취소 후 `holding_qty_after` 비교
- `check_and_restore_from_balance`에서 holdings 조회 공통 함수 추출
- `restore_position`의 임의 리스크 재생성 제거
- WS 체결통보 health gate 추가
- `auto_start` 기본 OFF
- cancel 로그 라벨 분리

#### 실제 수정 파일

| 파일 | 변경 |
|---|---|
| `src/strategy/live_runner.rs` | 핵심 로직 수정 |
| `src/infrastructure/cache/postgres_store.rs` | `active_positions` 확장 및 schema update |
| `src/presentation/routes/strategy.rs` | degraded/manual 상태 노출 |
| `src/main.rs` | startup health gating / auto_start default 조정 |

#### 핵심 함수

- `src/strategy/live_runner.rs:258` — `cancel_all_pending_orders`
- `src/strategy/live_runner.rs:446` — `check_and_restore_from_balance`
- `src/strategy/live_runner.rs:500` — `restore_position`
- `src/strategy/live_runner.rs:1589` — `execute_entry`

#### 스키마 확장

현재 이 저장소는 별도 `migrations/` 디렉터리가 아니라 `src/infrastructure/cache/postgres_store.rs` 내부 `migrate()`에서 스키마를 관리한다.

`active_positions`에 다음 필드를 추가한다.

- `or_high BIGINT NULL`
- `or_low BIGINT NULL`
- `or_source VARCHAR(10) NULL`
- `trigger_price BIGINT NULL`
- `original_risk BIGINT NULL`
- 필요 시 `entry_mode VARCHAR(20) NULL`

#### 검증

- cancel 후 체결내역 0건이어도 balance 보유 발생 시 포지션 복구
- WS 체결통보 health fail 시 거래 시작 차단
- 2026-04-15 사례 리플레이에서 orphan position 0건

### PR2 — 새 전략 모듈 추가

- `orb_vwap_pullback` 신설
- OR 15분 단일화
- VWAP 연결
- 방향 확인 + armed 상태
- 1분 반전 확인

### PR3 — 실행 정책 통일 + UI

- 시장성 지정가 기반 진입
- timeout/slippage 공통화
- 전략 상태 패널 최소 추가

표시 항목:

- OR high/low
- VWAP
- direction
- armed
- entry zone
- cutoff
- active instrument
- degraded/manual state

### PR4 — 리플레이 + 모의투자 + 소액 실거래 전환

- 2026-04-14, 2026-04-15 자동 리플레이
- 모의투자 5영업일 연속 운영
- 이상 없으면 소액 전환

## 완료 기준

### PR1 완료 기준

- cancel → `filled_qty == 0` → balance 보유 > 0 시 포지션 반드시 복구
- WS 체결통보 비정상 상태에선 거래 시작 안 함
- 재시작 복구 실패 시 임의 스탑 생성 없이 `manual_intervention_required`

### PR2 완료 기준

- 새 전략이 하루 최대 1회 진입 규칙으로 동작
- 오후 재진입 0건

### PR3 완료 기준

- 새 전략 기준 백테스트와 라이브가 동일 `ExecutionPolicy` 사용
- trigger / timeout / slippage budget 규칙 일치

### PR4 완료 기준

- 2026-04-14, 2026-04-15 리플레이에서 orphan position 0건
- 모의투자 5영업일 연속 안정 운영
- event_log 리플레이에서 미해명 상태 없음

## 2026-04-16 운영 원칙

P0가 장 전까지 끝나지 않으면:

- 실거래 자동매매는 시작하지 않음
- 가능하면 관찰 모드 또는 모의투자만 수행
- `auto_start`는 유지 OFF

P0가 끝나도 첫날은:

- 수동 시작
- 1종목만 운영
- 최대 1회 진입
- 장중 로그/잔고/UI 상태를 동시에 모니터링

## 확인 필요 사항

- `KIS_HTS_ID` 및 체결통보 AES health check를 장 시작 전 강제할지, degraded 모드만 허용할지 최종 결정
- 2026-04-16에는 `122630`만 운영할지, 방향에 따라 `114800`도 허용할지 운영정책 확정
- 시장성 지정가의 기본 슬리피지 한계:
  - `max_slippage_ticks`
  - `max_entry_drift_pct`
  - timeout `2초` vs `3초`

위 3건은 PR1/PR2 구현 중 결정하되, 2026-04-16 장 전에는 반드시 값이 고정되어 있어야 한다.
