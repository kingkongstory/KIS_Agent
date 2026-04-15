# 실전 투입 구현 계획

작성일: 2026-04-16  
목표: 현재 코드베이스를 **구현 후 실전 투입 가능한 상태**로 만들기 위한 구체적 실행 계획  
관련 문서:
- `docs/monitoring/2026-04-15.md`
- `docs/monitoring/2026-04-15-analysis.md`
- `docs/monitoring/pr1-fix-plan.md`
- `docs/monitoring/strategy-redesign-plan.md`
- `docs/strategy/backtest-live-parity-architecture.md`

## 1. 최종 목표

실전 투입의 기준은 "전략이 좋아 보인다"가 아니라 아래 세 가지다.

1. 시스템 상태와 KIS 실제 상태가 절대 분리되지 않는다.
2. 장애가 나면 자동으로 거래를 멈추고 사람이 개입할 수 있다.
3. 백테스트, 리플레이, 라이브 결과 차이를 설명할 수 있다.

초기 실전 버전은 **`orb_fvg_safe` 운영 모드**를 기준으로 한다.  
즉시 필요한 것은 새 전략 전면 교체가 아니라, 현재 전략을 **보수적 제약 + 강한 안전장치** 아래에서 운영 가능한 상태로 만드는 것이다.

## 2. 실전 투입 전 필수 완료 항목

아래 5개는 모두 `Blocker`다. 하나라도 미완료면 실전 `No-Go`다.

| 구분 | 항목 | 우선순위 | 완료 기준 |
|---|---|---|---|
| B1 | DB/프로세스 기동 안전성 | P0 | 실전 모드에서 DB 없으면 서버가 기동하지 않음 |
| B2 | WebSocket 런타임 생존성 | P0 | WS 장중 단절 시 재연결 또는 프로세스 재기동으로 복구 |
| B3 | 잔고/포지션 정합성 재검증 | P0 | 숨은 포지션 발생 시 즉시 감지하고 거래 중단 |
| B4 | 실전 운용 제약 강제 | P0 | 1종목, 1회, 수동 시작, 허용 종목 제한이 코드로 강제됨 |
| B5 | 리플레이/패리티 검증 가능 상태 | P0 | 사고일 리플레이와 패리티 백테스트가 재현 가능 |

## 3. 구현 범위

### Phase 1. 런타임 안전장치 완성

#### Task 1. 실전 모드 DB hard-fail

대상 파일:
- `src/config.rs`
- `src/main.rs`
- `src/infrastructure/cache/postgres_store.rs`

구현:
- `KIS_ENVIRONMENT=real` 또는 `KIS_ENABLE_REAL_TRADING=true`일 때 PostgreSQL 연결 실패 시 서버를 즉시 종료한다.
- `PostgresStore::new()`에 `acquire_timeout`, `connect_timeout`, `max_connections`를 명시한다.
- `replay`, `backtest`, `collect-*` CLI는 `expect()` panic 대신 명시적 오류 메시지와 종료 코드로 바꾼다.

수용 기준:
- 실전 모드 + DB down 상태에서 서버가 0초대에 실패하고 경고만 남긴 채 메모리 모드로 뜨지 않는다.
- `replay 122630 --date 2026-04-15`가 panic 없이 실패 원인을 출력한다.

#### Task 2. WS watchdog + fatal exit 정책

대상 파일:
- `src/infrastructure/websocket/connection.rs`
- `src/main.rs`
- `src/presentation/routes/monitoring.rs`

구현:
- `KisWebSocketClient`에 `last_message_at`, `last_tick_at`, `last_notification_at`, `retry_count`, `terminated` 상태를 노출한다.
- 장중 `N초` 이상 tick 부재 또는 `max_retries` 초과 시 `critical` 이벤트를 남기고 프로세스를 종료한다.
- `/api/v1/monitoring/health`에 WS 최신 수신 시각과 상태를 포함한다.
- 외부 supervisor 사용 전제를 문서화한다. Windows 기준 `nssm` 또는 작업 스케줄러 재시작 규칙을 둔다.

수용 기준:
- WS 강제 차단 시 `health`에 stale 상태가 보이고, 임계 초과 시 프로세스가 종료된다.
- 기존 2026-04-15의 "좀비 프로세스 + 분봉 0건" 상태가 재현되지 않는다.

#### Task 3. 주기적 잔고/포지션 reconciliation

대상 파일:
- `src/strategy/live_runner.rs`
- `src/presentation/scheduler.rs`
- `src/presentation/routes/strategy.rs`

구현:
- 30~60초 주기로 KIS 잔고와 `active_positions`, `state.current_position`을 비교한다.
- 불일치가 있으면 `manual_intervention_required`로 전환하고 신규 진입을 전부 막는다.
- `cancel_fill_race_detected`, `balance_reconcile_mismatch`, `hidden_position_detected` 이벤트를 분리 기록한다.

수용 기준:
- DB 포지션 0, KIS 보유 > 0 상황을 강제로 만들면 1분 내 `critical`로 잡히고 러너가 중단된다.
- 수동 개입 상태에서는 반대 종목도 새 진입을 시도하지 않는다.

### Phase 2. 실전 운용 제약 강제

#### Task 4. 실전 안전 모드(`orb_fvg_safe`) 추가

대상 파일:
- `src/strategy/orb_fvg.rs`
- `src/strategy/live_runner.rs`
- `src/main.rs`
- `src/presentation/routes/strategy.rs`

구현:
- 실전 모드 기본값을 `single_15m`로 고정한다. 5m/30m stage는 UI 참고용으로만 남긴다.
- `max_daily_trades`를 실전 모드에서 `1`로 강제한다.
- 신규 진입 마감은 더 이른 시간으로 분리 가능하도록 `entry_cutoff_real`을 환경변수화한다.
- `KIS_ALLOWED_CODES`로 실전 허용 종목을 제한한다. 초기값은 `122630` 1종목만 권장한다.

수용 기준:
- 실전 모드에서 5m stage 신호로는 진입하지 않는다.
- 1회 진입 후 같은 날 두 번째 진입이 코드에서 거부된다.

#### Task 5. 수동 시작 강제 + 실전 가드 변수 도입

대상 파일:
- `src/main.rs`
- `src/presentation/routes/strategy.rs`
- `.env.example`

구현:
- `KIS_AUTO_START=false`를 실전 기본값으로 유지한다.
- `KIS_ENABLE_REAL_TRADING=true`가 아니면 실전 주문 경로를 막는다.
- `KIS_ALLOWED_CODES`, `KIS_MAX_DAILY_TRADES_TOTAL`, `KIS_REQUIRE_DB_IN_REAL`, `KIS_WS_STALE_SECS`, `KIS_RECONCILE_SECS`를 추가한다.

수용 기준:
- 실전 계정이어도 `KIS_ENABLE_REAL_TRADING` 없으면 start 요청이 거부된다.
- 허용되지 않은 종목은 UI/API에서 시작되지 않는다.

### Phase 3. 검증 도구와 운영 절차 완성

#### Task 6. replay/parity 경로를 배포 검증 절차로 고정

대상 파일:
- `src/main.rs`
- `src/strategy/backtest.rs`
- `src/strategy/parity/*`
- `docs/monitoring/*`

구현:
- `2026-04-14`, `2026-04-15`, 최근 정상일 1일을 고정 검증 세트로 둔다.
- `replay` 결과에 `legacy`, `parity-legacy`, `parity-passive` 비교표를 항상 출력한다.
- DB 연결 실패 시 즉시 원인 출력 후 종료하도록 보완한다.

수용 기준:
- 사고일 리플레이가 정상 수행된다.
- 패리티 결과와 라이브 로그 차이를 설명할 수 있다.

#### Task 7. 배포 문서/체크리스트 작성

대상 파일:
- `docs/monitoring/`
- `AGENTS.md` 필요 시 링크 추가

구현:
- 장 시작 전 체크리스트, 장중 중단 조건, 장 종료 후 확인 절차, 롤백 절차를 문서화한다.
- 배포 SHA, 빌드 시각, `.env` 핵심 값, supervisor 설정을 기록한다.

수용 기준:
- 운영자는 코드 없이 문서만 보고 기동/중지/롤백을 수행할 수 있다.

## 4. 실전 투입 전 검증 순서

### 4.1 코드 검증

반드시 통과해야 하는 명령:

```bash
cargo test
cargo build --release
cd frontend && npm run build
.\target\release\kis_agent.exe replay 122630 --date 2026-04-15 --rr 2.5
.\target\release\kis_agent.exe replay 114800 --date 2026-04-15 --rr 2.5
.\target\release\kis_agent.exe backtest 122630 --days 5 --parity passive --multi-stage
```

### 4.2 모의투자 burn-in

실전 전 최소 조건:

1. 모의투자 3영업일 연속 무사고
2. `manual_intervention_required=0`
3. `cancel_fill_race_detected=0`
4. orphan position 0건
5. WS critical 종료 0건

### 4.3 실전 파일럿

실전 첫날 조건:

1. `122630` 단일 종목
2. 하루 최대 1회 진입
3. 수동 시작
4. 체결 직후 HTS/MTS 잔고와 UI 상태를 직접 대조
5. 첫 거래일은 장 마감 전 수동 검토 후 종료

## 5. Go / No-Go 기준

### Go

- Phase 1~3 완료
- 고정 리플레이 3세트 통과
- 모의투자 3영업일 연속 무사고
- 실전 체크리스트 문서화 완료
- 배포 SHA 고정 완료

### No-Go

- DB 연결 문제 미해결
- WS stale/critical 시 자동 복구 또는 종료가 없음
- replay가 아직 실패
- `max_daily_trades=1`, 허용 종목 제한, 수동 시작 강제가 코드에 없음
- 새 코드가 미커밋 상태로 남아 있음

## 6. 구현 순서 제안

1. DB hard-fail + CLI 오류 처리
2. WS watchdog + health 노출
3. reconciliation 주기 작업
4. 실전 운용 제약(`single_15m`, 1종목, 1회)
5. replay/parity 검증 안정화
6. 배포 체크리스트/롤백 문서
7. 모의투자 burn-in
8. 실전 파일럿

## 7. 이번 계획의 운영 원칙

이 문서의 목표는 **바로 전략을 화려하게 바꾸는 것**이 아니다.  
먼저 현재 시스템을 **실패하면 멈추고, 상태가 어긋나지 않고, 재현 가능한 시스템**으로 만든다.  

`orb_vwap_pullback` 본 구현은 그 다음 단계다. 현재 파일은 스켈레톤이므로, 첫 실전 투입 기준은 `orb_fvg_safe` 운영 모드로 잡는다.
