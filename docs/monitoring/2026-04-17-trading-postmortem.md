# 2026-04-17 거래 포스트모텀 (paper burn-in 2일차)

> 종목 122630 (KODEX 레버리지) 2건 거래, 누적 **-1.05%** 손실. 여러 데이터 소스
> 교차검증으로 실패의 **근본 원인** 5가지를 식별. 즉시 follow-up 조치와 중장기
> 전략 재검토 권고를 담는다.

## 0. 오늘 요약

| 항목 | 값 |
|---|---|
| 거래 | **2건** (122630 Long 2회, 114800 0건) |
| 승/패 | 0W / 2L |
| 누적 pnl | **-1.05%** |
| 1차 거래 | 09:55~10:02, **-0.70%** StopLoss |
| 2차 거래 | 11:00~11:31, **-0.35%** TimeStop |
| 미진입 신호 | 3건 (13:35 abort_entry) |
| 시장 조건 | 122630 종일 -0.77% (하락 추세) |
| 부가 사고 | 11:27~11:28 KIS 네트워크 장애 → watchdog exit(11) → 재시작 시 P0 우회 청산 |

## 1. 거래 상세

| # | 시각 | entry → exit | SL / TP | slip(entry/exit) | hold | 사유 | pnl |
|---|---|---|---|---|---|---|---|
| 66 | 09:55:41 → 10:02:43 | 102,736 → 102,015 | 102,046 / 104,461 | -19 / **+31** | 7분 | StopLoss | **-0.70%** |
| 67 | 11:00:45 → 11:31:43 | 102,434 → 102,070 | 101,894 / 103,784 | -56 / 0 | 31분 | TimeStop | **-0.35%** |
| (미진입) | 13:35:00 | 신호 @101,992 | 101,385 / 103,509 | — | — | abort_entry (fill_timeout_or_cutoff) | 0 |

**신규 적용 효과 (권고 #1 검증)**: 1차 거래의 `exit_slippage=+31` 실측
(SL 102,046 vs 실체결 102,015, 31원 추가 손실). 이전 구현이라면 0으로 기록됐을
수치. 2차는 TimeStop이라 0 (시장가 intended 없음).

## 2. 신호 품질 분석 — 매우 낮음

`event_log.metadata` JSON 추출:

| # | stage | B시각 | C시각 | gap_size | b_body_ratio | **or_breakout_pct** | b_volume |
|---|---|---|---|---|---|---|---|
| 1차 | 5m | 09:40 | 09:50 | 0.25% | 1.03 | **+0.034%** ← 거의 0 | 0 |
| 2차 | 5m | 10:25 | 10:55 | 0.19% | 1.64 | **-0.054%** ← **음수**, OR 돌파 전 | 0 |
| 3차 | 5m | 13:15 | 13:30 | 0.31% | 1.31 | **-0.706%** ← 큰 음수 | 0 |

**세 건 모두 or_breakout_pct ≤ +0.034%**. 전략 명세상 "OR 상단 상향 돌파 후
FVG 리트레이스" 가 Long 진입 조건이지만, **실제로는 OR 돌파 없이 신호 생성**.
2/3차는 OR 상단 아래(음수). 신호 품질 결함.

## 3. Yahoo replay 대조 — **라이브 2건 vs Yahoo 0건**

`./target/release/kis_agent.exe replay 122630 --date 2026-04-17 --rr 2.5` 결과:

```
legacy_backtest:         0 거래 / 0.00%
parity_backtest_legacy:  0 거래 / 0.00%
parity_backtest_passive: 0 거래 / 0.00%
```

**라이브가 2건 진입한 동일 시각의 Yahoo 5m 분봉으로는 FVG 자체가 감지 안 됨.**
즉 라이브만의 가짜 신호.

## 4. OR 범위 — 라이브 vs Yahoo 데이터 불일치

| 단계 | 라이브 DB (ws 집계) | Yahoo 5m |
|---|---|---|
| 5m | 102,060 ~ **102,740** | 102,050 ~ **102,830** |
| 15m | 101,400 ~ **102,740** | 101,400 ~ **102,830** |
| 30m | 101,400 ~ **102,740** | 101,400 ~ **102,830** |

**OR 상단 90원 차이**. 라이브 WebSocket 집계의 high 가 Yahoo 공식 5m 분봉보다
낮음. 동일 거래소 동일 시각인데 값이 다르다 → **라이브 분봉 품질 결함 (high tick 일부 누락)**.

## 5. 1차 거래 가격 흐름 재구성 (Yahoo 기준)

```
09:40 O=102,330 H=102,810 L=102,250 C=102,760  ← B 캔들
09:45 O=102,775 H=103,140 L=102,750 C=103,065  ← A 캔들 (Yahoo high 103,140)
09:50 O=103,065 H=103,085 L=102,535 C=102,640  ← C 캔들 (Yahoo low 102,535)

Bull FVG 조건: B.high(102,810) < C.low(102,535) → Yahoo 기준 성립 X
                                                   ↑ 라이브는 성립했다고 판정

09:55 진입 시도 → 10:00 L=101,955 (SL 102,046 하회) → SL 트리거 7분 만에
```

라이브는 "B.high < C.low" 조건으로 가짜 FVG 판정. 진입 후 즉각 하락.

## 6. 2차 거래의 Reconcile 이상 행동 — **새 결함 발견**

```
11:00:03 entry_signal Long @102,490
11:00:??  지정가 발주 (execute_entry 내부 30초 대기 진입)
11:00:37 reconcile_once → runner=0, KIS=92 → mismatch critical
           ↓ 그러나 실제 hidden position 이 아니라 "진입 진행 중"
11:00:41 cancel_fallback (30초 대기 타임아웃 후 취소 시도)
11:00:44 cancel_fill_race_detected — 실제 +92주 체결 확인
11:00:44 entry_executed (정상 진입 확정)
11:00:50 auto_close_skipped (warn) ← v3+ P0 정상 작동, 포지션 보존
```

**핵심 진단**:
- `execute_entry` 30초 대기 루프 동안 KIS 서버는 이미 체결 처리
- runner 의 `current_position` 은 체결 확인 후에만 update
- 이 "체결 완료 - runner update" 시간 갭이 1분 주기 `reconcile_once` 와 겹치면
  **false mismatch → manual 전환**

v3+ fixups F1.1 의 streak 보호는 `confirm_reconcile_qty` 가 **Err (fetch 실패)**
인 경우만 다룬다. 이 케이스는 confirm 이 `Ok(92)` 반환하므로 곧바로 mismatch
분기 → manual 전환. **차단 못 함.**

## 7. 2차 거래는 결과적으로 TimeStop 정상

Yahoo 11:00~11:30 5m 흐름:
```
11:00 O=102,455 H=102,860 L=102,350 C=102,765
11:05 O=102,770 H=102,855 L=102,185 C=102,285
11:10 O=102,295 H=102,685 L=102,215 C=102,655
11:15 O=102,650 H=102,720 L=102,385 C=102,385
11:20 O=102,405 H=102,545 L=102,305 C=102,325
11:25 O=102,305 H=102,445 L=102,155 C=102,175
11:30 O=102,170 H=102,255 L=102,010 C=102,080
```

- **1R = 102,490 - 101,950 = 540 원**
- **1R 도달 목표 = 102,490 + 540 = 103,030**
- 최고 11:00 high **102,860** 까지만 도달 → 1R 미달
- 31분 후 TimeStop (30분 임계 초과)

**즉 P0 우회 사고로 manual 보호됐어도 TimeStop 이 같은 시각 발동했을 것.**
이번 재시작 사고의 실질 손실 증가분 = 0. 단 **사고 경로 자체는 버그**.

## 8. 장중 네트워크 장애 + 재시작 → P0 우회

### 타임라인

```
11:00:37 balance_reconcile_mismatch (false, 위 §6)
11:00:50 v3+ P0 → auto_close_skipped (manual 보호 작동)
         ↓ 포지션 92주 보존
11:27:04 KIS 네트워크 장애 시작 (HTTP/WS 모두 timeout)
11:28:40 ws-watchdog exit(11) — tick stale 121초 임계 초과
11:31:04 사용자 지시로 backend 재시작
11:31:43 exit_market TimeStop @102,070   ← P0 우회 청산
```

### 결함: `manual_intervention_required` 메모리 전용

- v3+ 까지: `RunnerState.manual_intervention_required` in-memory 만
- `active_positions` DB 에는 이 플래그 미존재
- watchdog 강제 종료 → 메모리 state 손실
- 재시작 후 `check_and_restore_position` 이 포지션만 복원, manual=false (기본값)
- `manage_position` stop 분기가 manual=false 보고 정상 청산 경로 진입

### 조치 (이미 적용됨 — 11:49 KST)

DB 영속화 긴급 follow-up:
1. `active_positions.manual_intervention_required BOOLEAN` 컬럼 추가 (ALTER TABLE)
2. `PostgresStore::set_active_position_manual` 신규
3. `trigger_manual_intervention` + `reconcile_once` 두 분기에서 DB 저장
4. `check_and_restore_position` 에서 복원 시 state/lock(`ManualHeld`)/stop_flag 일괄 설정
5. 신규 이벤트: `manual_intervention_restored` (critical)
6. save 실패 시 `manual_flag_persist_failed` (critical)

## 9. 시장 조건

- **122630 종일**: 09:00 open 102,710 → 15:00 close 101,915 = **-0.77%**
- 종일 **하락 추세** → Long-only 전략 불리
- **114800 진입 0건** — 상승장 방어 ETF 로서 122630 역방향 기회 있어야 했지만 OR 범위 1,438~1,443 (폭 5원) 너무 좁아 FVG 성립 조건 미충족 가능성. 별도 조사 필요.

## 10. 실패 근본 원인 랭킹

| # | 원인 | 증거 | 가중치 |
|---|---|---|---|
| **1** | **가짜 FVG 신호** (라이브만 감지) | or_breakout_pct ≤ 0.034%, Yahoo replay 0건 | 최대 |
| **2** | **WebSocket 분봉 품질 결함** | OR high 라이브 102,740 vs Yahoo 102,830 (90원 차), b_volume=0 지속 | 큼 |
| **3** | **하락 추세에서 Long-only 강행** | 종일 -0.77%, 2건 모두 loss | 큼 |
| **4** | **Reconcile false mismatch (새 결함)** | 11:00:37 "진입 진행 중" runner state 를 hidden position 으로 오판 | 중 |
| **5** | **ExecutionPolicy 의 반복 cancel/fill race** | 2건 모두 30초 대기 → cancel → race | 중 |
| **6** | **OR 돌파 가드 부재/약함** | or_breakout_pct 0 근처, 음수도 진입 | 중 |

## 11. 권고 조치

### A. 긴급 (다음 거래일 전)

| # | 조치 | 파일 | 변경 라인 |
|---|---|---|---|
| A-1 | 라이브 분봉 vs Yahoo 비교 검증 스크립트 (매일 장 마감 후 자동) | 신규 bin 또는 cron | ~80 |
| A-2 | Reconcile 의 position_lock 상태 인식 — `Pending(code)` 면 해당 종목 skip | `strategy.rs:reconcile_once` | ~15 |
| A-3 | or_breakout_pct 임계 가드 (기본 0.1% 또는 0.2%) | `strategy/parity/signal_engine.rs` 또는 `orb_fvg.rs` | ~20 |

### B. 중요 (1주 내)

| # | 조치 | 사유 |
|---|---|---|
| B-1 | b_volume 거래량 누락 버그 수정 | candle_aggregator 깊은 버그 (2026-04-15 분석 이래 미수정) |
| B-2 | ExecutionPolicy 재검토 | 지정가 30초 대기 → cancel 패턴의 race 다빈도 (오늘 2/2) |
| B-3 | WebSocket tick 품질 audit | 라이브 high 90원 누락 조사 (buffer/aggregation 로직) |

### C. 전략/운영 재검토

| # | 조치 | 사유 |
|---|---|---|
| C-1 | 일일 사전 방향 필터 (상승 추세일 때만 122630, 하락 추세일 때만 114800) | 종일 Long-only 하락장 진입 방지 |
| C-2 | 114800 OR 범위 좁을 때 신호 생성 조건 재검토 | 오늘 OR 5원폭에도 FVG 진입 시도 없었던 이유 |
| C-3 | Paper burn-in 기간 3일 → 5일로 연장 | 2일차 -1.05%, 신뢰성 확보 부족 |

## 12. 사고 타임라인 한 줄 요약

**"낮은 품질의 가짜 FVG 신호로 라이브만 2회 진입 → 시장 하락 추세에서 1차 SL
-0.70% + 2차 TimeStop -0.35% → 2차 진입 중 reconcile false mismatch 로 manual
오탐 → 네트워크 장애 watchdog 재시작으로 P0 우회 청산 (실질 loss 증가는 없음)
→ 긴급 DB 영속화 follow-up 적용."**

## 13. 적용된 수정 (오늘 중)

- **권고 #1**: SL 계열 exit_slippage 실측 (live_runner.rs:3308)
- **권고 #2**: trades.environment 하드코딩 제거 (live_runner.rs:3299)
- **WS retry 리셋**: 구독 확인 후 `retry_count=0` 리셋 + max_retries 20 (connection.rs:163, 300)
- **긴급 DB 영속화**: `active_positions.manual_intervention_required` 컬럼 + 복원 경로

## 14. 추가 논의 주제

- **v3+ fixups v2 의 P0/P1/F1~F4/H 가 오늘 진짜로 검증됐는가?**
  - F3 (Held(other) 보존): 트리거 없음
  - F1.1 streak: 이번 mismatch 는 fetch Ok(mismatch) 라 streak 경로 안 거침
  - P0 auto_close_skipped: 11:00:50 1회 작동 확인 ✓
  - **DB 영속화 (긴급 follow-up)**: 적용은 됐지만 실전 검증은 아직. 다음 manual 전환 시나리오에서 확인
- **오늘이 첫 라이브였던 신규 코드의 사고 기여도**: 직접 손실 기여 0. 단 reconcile
  오탐 + watchdog exit 조합이 경로 자체의 robustness 부족을 드러냄.
