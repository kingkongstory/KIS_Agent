# CLAUDE.md

이 파일은 Claude Code (claude.ai/code)가 이 저장소에서 작업할 때 참고하는 지침입니다.

## 언어 규칙

- **사용자와의 모든 대화는 반드시 한국어로 진행한다.**
- 코드 주석, 커밋 메시지, PR 설명 등 문서도 한국어를 기본으로 한다 (변수명/함수명 등 코드 식별자는 영문).

## 프로젝트 개요

KIS_Agent는 **한국투자증권 Open Trading API**를 활용한 자동 주식매매 Rust 웹 애플리케이션이다. KIS REST/WebSocket API를 래핑하여 자동 주문 실행, 포트폴리오 관리, 실시간 시세 스트리밍을 제공한다.

## 빌드 및 개발 명령어

```bash
cargo build                          # 디버그 빌드
cargo build --release                # 릴리스 빌드
cargo run                            # 애플리케이션 실행
cargo test                           # 전체 테스트 실행
cargo test <test_name>               # 단일 테스트 실행
cargo test --lib                     # 유닛 테스트만 실행
cargo test --test <integration_test> # 특정 통합 테스트 실행
cargo clippy                         # 린트
cargo fmt                            # 코드 포맷팅
cargo fmt -- --check                 # 포맷 검사 (수정 없이)
```

## KIS Open Trading API 레퍼런스

**공식 저장소**: https://github.com/koreainvestment/open-trading-api
**상세 문서**: [docs/README.md](docs/README.md) — API 레퍼런스, 기술적 지표, 매매 전략

### 기본 URL

| 환경 | REST API | WebSocket |
|---|---|---|
| 실전투자 | `https://openapi.koreainvestment.com:9443` | `ws://ops.koreainvestment.com:21000` |
| 모의투자 | `https://openapivts.koreainvestment.com:29443` | `ws://ops.koreainvestment.com:31000` |

### 인증 흐름

1. KIS 개발자 포털(`apiportal.koreainvestment.com`)에서 `appkey`와 `appsecret` 발급
2. `/oauth2/tokenP`로 POST 요청하여 Bearer 액세스 토큰 획득 (유효기간 ~24시간, 6시간 이내 재발급 시 동일 토큰 반환)
3. WebSocket용: `/oauth2/Approval`로 POST 요청하여 `approval_key` 획득
4. Hashkey는 `/uapi/hashkey` — 현재 필수 아니지만 향후 필수화 가능

### REST 표준 헤더

```
Content-Type: application/json
authorization: Bearer {access_token}
appkey: {appkey}
appsecret: {appsecret}
tr_id: {transaction_id}       # API 오퍼레이션 식별자
custtype: P                   # P = 개인
```

- `tr_id`는 실전/모의투자에서 동일 기능이라도 코드가 다름 (예: 현금주문 `TTTC0802U` vs `VTTC0802U`)

### 주요 API 엔드포인트 카테고리

모든 REST 경로는 `/uapi/` 하위:

| 카테고리 | 경로 접두사 | 주요 기능 |
|---|---|---|
| 국내주식 | `/uapi/domestic-stock/` | 주문, 취소, 시세조회, 잔고, 차트, 투자자/회원 통계 |
| 해외주식 | `/uapi/overseas-stock/` | 국내주식과 동일 패턴의 해외 시장 기능 |
| 국내선물옵션 | `/uapi/domestic-futureoption/` | 파생상품 거래 |
| 해외선물옵션 | `/uapi/overseas-futureoption/` | 해외 파생상품 |
| 채권 | `/uapi/domestic-bond/` | 채권 거래 |

### 속도 제한

- 신규 계정: 최초 3일간 **초당 3건**, 이후 표준 할당
- 토큰 발급: **분당 최대 1회**
- 모의투자는 실전투자보다 REST 호출 제한이 엄격함

## 아키텍처

Rust 워크스페이스로 구성된 웹 애플리케이션이며, 아래 계층으로 구분된다:

- **API 클라이언트 계층**: KIS REST 엔드포인트 및 WebSocket 스트림을 래핑하는 타입 기반 HTTP 클라이언트. 인증, 토큰 갱신, 요청 서명 처리.
- **매매 엔진**: 주문 관리(매수/매도/취소/정정), 포지션 추적, 전략 실행 등 핵심 로직.
- **웹 계층**: Axum 기반 HTTP 서버. 대시보드와 매매 에이전트 제어 API 제공.
- **실시간 계층**: 실시간 시세(호가, 체결) WebSocket 클라이언트 및 구독 관리.

### 핵심 설계 원칙

- **클린 아키텍처 준수** — domain/application/infrastructure/presentation 계층 분리, 의존성은 안쪽으로만
- **OOP 지향** — 구조체 + impl 블록으로 책임 캡슐화, trait으로 인터페이스 추상화
- 모든 KIS API 타입은 serde (역)직렬화가 가능한 Rust 구조체로 강타입화
- 환경(실전/모의투자)은 명시적으로 관리 — `tr_id` 코드가 환경에 따라 변경됨
- 토큰 생명주기(24시간 만료, 분당 1회 제한)에 대응하는 중앙 토큰 관리자와 자동 갱신 필수
- WebSocket 재연결 로직 필수 — KIS 연결은 "No close frame received" 오류로 끊어질 수 있음
- **DB 세션 timezone 강제 KST** — `PostgresStore::new`는 `PgPoolOptions::after_connect`로 모든 sqlx 연결에 `SET TIME ZONE 'Asia/Seoul'`을 실행한다. `minute_ohlcv.datetime`이 `timestamptz`라 NaiveDateTime ↔ timestamptz cast가 세션 TZ를 따르며, KST가 아니면 백테스트의 09:00~15:30 조회가 0건이 된다 (2026-04-10 사고 분석: [docs/monitoring/2026-04-10.md](docs/monitoring/2026-04-10.md))

## 자동매매 시스템

### ORB+FVG 전략 흐름

1. **OR 수집** (09:00~09:15): Opening Range 고가/저가 산출. **2026-04-18 v3 불변식**: paper/real 모두 `allowed_stages=Some(["15m"])` 단일 stage 고정 (기본값 `LiveRunnerConfig::default()` 에서 강제). 5m/30m 은 `or_stages` 관찰 배열에만 노출되며 진입 경로 차단.
2. **FVG 탐색** (09:15~15:20): 5분봉에서 3캔들 갭 패턴 탐지 + **OR 돌파 항상 필수** (`require_or_breakout=true` 고정 — 기존 `trade_count==0` 조건부는 2026-04-17 2 차 거래 오탐의 원인이라 v3 에서 제거).
3. **진입**: FVG 리트레이스 시 **지정가 매수 (gap.top 기준, 30초 체결 대기)**
4. **청산**: TP 지정가 매도 (KRX 자동 체결) / SL·트레일링·시간스탑은 시장가

**stage 불변식 관통 범위**: 라이브 + `backtest.rs:run/run_session_reset/run_dynamic_target` + `parity_backtest.rs` + `orb_fvg.rs` + `main.rs` 의 `--stages` 옵션 (기본 `15m`) 까지 동일 규칙. 단일 stage 경로는 `--stages "5m,15m"` 로 2+ 개 지정하면 `require_single_stage()` 가 즉시 에러로 거부 (multi-stage / parity / dual-locked / replay 경로는 CSV 필터 그대로 허용).

### 주문 방식 (하이브리드)

| 동작 | 주문 유형 | 이유 |
|---|---|---|
| 진입 (매수) | **지정가** (`ORD_DVSN=00`, **gap.top 가격**) | 슬리피지 0, 백테스트 zone 터치 조건과 일치 (2026-04-14 변경) |
| 익절 (TP) | 지정가 (`ORD_DVSN=00`) | 정확한 목표가 체결, 슬리피지 0 |
| 손절 (SL) | 시장가 | 즉시 청산 (TP 취소 후 실행) |
| 장마감 청산 | 시장가 | TP 취소 후 강제 청산 |

**지정가 매수 상세**: 30초 체결 대기 루프 → 미체결 시 자동 취소 → 다음 FVG 탐색. cancel 3회 실패 시 `cancel_all_pending_orders()` fallback 동작.

### 포지션 잠금 (PositionLockState)

두 종목(122630/114800) 간 동시 진입 방지용 공유 lock:

| 상태 | 의미 | 전이 |
|---|---|---|
| `Free` | 잠금 없음 | 어느 종목이든 진입 가능 |
| `Pending{code, preempted}` | 지정가 발주 후 체결 대기 중 | 다른 종목 진입 시 `preempted=true` 설정 → 대기 종목 즉시 취소 |
| `Held(code)` | 포지션 보유 확정 | 다른 종목 진입 차단 (청산 시 Free) |
| `ManualHeld(code)` | 포지션 보유 + 수동 개입 플래그 | 자동 청산 전 경로 차단 (P0 / SL / TP / 트레일 모두). `active_positions.manual_intervention_required=true` 로 DB 영속화되어 재시작/watchdog 재기동 후에도 보존 |

**선점(preempt) 메커니즘**: `wait_execution_notice()`의 `tokio::select!` 에 200ms 폴링 branch로 preempted 플래그 감지 (live_runner.rs `wait_preempted` 함수).

**reconcile Pending/ManualHeld 가드**: `should_skip_reconcile_for_lock()` helper 가 현재 종목이 `Pending(self)` 또는 `ManualHeld(self)` 상태면 reconcile 판정을 skip (지정가 발주 ~ 체결 확인 과도 상태에서 `runner_qty=0, KIS_qty>0` 가 manual 승격으로 오판되던 2026-04-17 11:00 사고 재발 방지). `reconcile_once` 가 `classify_reconcile_outcome` 과 함께 실경로에서 이 helper 를 직접 호출.

### 데이터 소스 분리 (v3 canonical 5m)

- **OR 범위 계산 / FVG 신호 탐색**: `fetch_canonical_5m_candles()` 가 반환하는 **단일 canonical 5m 시퀀스**로 둘 다 판정 (2026-04-17 v3 불변식 #2). 기존 `is_realtime` flag 기반 2-배열 분리는 granularity 혼재(WS 1 분봉 + Yahoo 5 분봉) + 배열 간 의미 차이로 FVG 오탐 유발 → 폐기.
- **merge precedence**:
  - **과거 완료 버킷**: Yahoo (`interval_min=5`) 백필 **우선**. 없을 때만 WS 1 분봉 집계로 보완. 근거: postmortem (docs/monitoring/2026-04-17-trading-postmortem.md) 상 WS 집계 OR high 가 Yahoo 대비 ±90 원 괴리 — Yahoo 가 진실.
  - **현재 진행 중 버킷**: WS 집계만 사용 (Yahoo 는 장중 데이터 아직 없음).
- `CompletedCandle.interval_min: u32` 필드가 granularity 를 태그. `upsert_completed_bar()` 는 `(time, interval_min)` 복합키로 merge.
- 네이버 백필 데이터는 종가만 제공(OHLC 동일) → FVG 탐색에 부적합.

### 서버 재시작 복구

- **OR 범위**: `daily_or_range` 테이블에서 당일 OR 로드
- **활성 포지션**: `active_positions` 테이블에서 포지션 + TP 주문번호 복구
- **이전 TP 지정가**: 저장된 주문번호로 취소 → 새 TP 재발주
- **분봉 데이터**: `minute_ohlcv`에서 당일 분봉 프리로딩
- OR 데이터 없으면 네이버 금융에서 자동 백필
- **Orphan 매수 주문 정리** (2026-04-14 추가): `check_and_restore_position()`의 `Ok(None)` 분기에서 `cancel_all_pending_orders()` 호출. `taskkill /F`/SIGKILL/패닉 등 모든 비정상 종료 후 재시작 시 KIS에 남은 미체결 매수 주문을 자동 취소. `inquire-psbl-rvsecncl` API로 조회.

### Graceful Shutdown (2026-04-14 추가)

`main.rs`에 `tokio::signal::ctrl_c()` + Unix `SIGTERM` 핸들러. 신호 수신 시 `StrategyManager::stop_all()` → 모든 러너에 stop 신호 → 체결 대기 루프 즉시 탈출 → cancel → `wait_all_stopped(40s)` → axum graceful shutdown.

**제약**: Windows `taskkill /F`와 SIGKILL은 OS 레벨 강제 종료이므로 커버 불가. 이 경우 재시작 시 orphan 정리 로직이 안전망 역할.

### DB 테이블

| 테이블 | 용도 |
|---|---|
| `minute_ohlcv` | 실시간 1분봉 (WebSocket 집계 + 네이버 백필) |
| `daily_ohlcv` | 일봉 히스토리 |
| `trades` | 거래 기록 (진입/청산 가격, 사유, 손익) |
| `active_positions` | 활성 포지션 (TP 주문번호 포함, 재시작 복구용) |
| `daily_or_range` | 일별 OR 고가/저가 |

### 운용 종목

- KODEX 레버리지 (122630): 상승장 Long
- KODEX 인버스 (114800): 하락장 Long
- Short 주문 없음 (모의투자 공매도 불가, 반대 ETF가 담당)
- 종목당 500만원 배정 (5:5 배분)

### 전략 파라미터 (OrbFvgConfig, `src/strategy/orb_fvg.rs:41-59`)

- **RR 비율: 2.5** (손익비 1:2.5)
- OR: 09:00~09:15, **stage 는 15m 단일 고정** (2026-04-18 v3 불변식)
- 진입 마감: 15:20 (지정가 대기 고려해 실제 컷오프 15:19:30), 강제 청산: 15:25
- **트레일링: 0.05R (5%R)**, **본전스탑: 0.15R (15%R)** — 매우 공격적 설정 (2026-04-11 `360f167` 파라미터 최적화)
- FVG 유효: 6캔들 (30분)
- 일일 최대 손실: -1.5%, 최대 거래: 5회 (단, 실전은 `max_daily_trades_override=1` 로 종목당 1 회)
- Long only (공매도 금지)

### 일일 운영 절차

매 거래일 장 마감 후 (15:30 이후) Yahoo에서 **당일 5분봉만 받아 DB에 추가**한다. `range=1d`라 73 캔들/종목만 호출되며, UPSERT로 안전. **DB는 매일 1일치씩 영구 누적**되며 60일 윈도우 슬라이딩이 아니다 → 시작일 이전 과거를 잃지 않는다.

```bash
# 1. 백테스트용 5분봉 당일 추가 — Yahoo Finance range=1d (당일 73캔들/종목)
./target/release/kis_agent.exe collect-yahoo --codes "122630,114800" --interval 5m --range 1d

# 2. 당일 결과 재현 (라이브 15m 단일 stage 와 같은 규칙)
./target/release/kis_agent.exe replay 122630 --date 2026-04-18 --rr 2.5 --stages 15m

# 3. 누적 백테스트 (단일 stage 경로 — 라이브 불변식 동일)
./target/release/kis_agent.exe backtest 122630 --days 365 --rr 2.5 --stages 15m

# 4. multi-stage 분석용 (진입 경로 아님, 관찰 목적만)
./target/release/kis_agent.exe backtest 122630 --days 365 --rr 2.5 --stages "5m,15m,30m" --multi-stage --dual-locked
```

**`--stages` 규칙**: 기본 `15m`. 단일 경로(`run`/`run_session_reset`/`run_dynamic_target`)는 정확히 1 개 값만 허용 (2+ 개면 `require_single_stage()` 에러). `--multi-stage`/`--dual-locked`/replay/parity 경로는 CSV 필터 그대로 수용.

**자동화 (Windows Task Scheduler 권장)**:
```cmd
schtasks /Create /SC WEEKLY /D MON,TUE,WED,THU,FRI /TN "KIS_Yahoo_Daily" /TR "C:\workspace_google\KIS_Agent\target\release\kis_agent.exe collect-yahoo --codes 122630,114800 --interval 5m --range 1d" /ST 15:35
```
평일 15:35에 자동 실행. 거래일 휴장 체크는 yahoo가 빈 응답으로 처리하므로 별도 로직 불필요.

**최초 적재**: 빈 DB라면 한 번만 `--range 60d`로 60 영업일치 적재 후, 다음 날부터 `--range 1d` 일일 호출로 전환. 현재 DB에는 이미 2026-01-13 이후 60일치가 있음.

**데이터 소스 결정 (2026-04-10)**: 백테스트 5분봉은 **Yahoo Finance**를 사용한다. 네이버 분봉(`collect-minute-naver`)은 1분 종가만 제공하여 5분봉 OHLC를 만들면 1분 wick을 못 잡고 가짜 신호가 발생한다. Yahoo는 진짜 OHLC + KST timezone(`exchangeTz=Asia/Seoul`)을 제공하며 라이브 모니터링 OR과 일치한다 (114800 완전 일치, 122630은 5~15원 더 정확).

**Yahoo 5분봉의 한계**: 단일 호출 최대 `range=60d` (90d/6mo/1y는 yahoo가 명시 거부, max는 일봉 fall-back). 즉 시간 여행 불가. 매일 1d 호출로 누적해야 60일 윈도우 이전 일자도 영구 보존된다.

**1분봉은 사용하지 않는다.** Yahoo 1분봉도 받을 수 있으나 (`interval=1m`) range가 8일 한계이고, 백테스트 엔진의 `source_interval = 5_i16`이 5분봉 전용이라 백테스트에 미사용. 라이브는 WebSocket 실시간 적재로 충분.

## 사고/변경 이력 레퍼런스

| 날짜 | 사고/변경 | 문서 |
|---|---|---|
| 2026-04-10 | DB 세션 KST timezone 강제 (백테스트 09:00~15:30 조회 0건 사고) | [docs/monitoring/2026-04-10.md](docs/monitoring/2026-04-10.md) |
| 2026-04-14 | 지정가 매수 전환 + 2단계 lock + Graceful shutdown + orphan 정리 | [docs/monitoring/2026-04-14.md](docs/monitoring/2026-04-14.md) |
| 2026-04-17 | -1.05% 손실 + P0 우회 사고 (2 차 거래 OR 돌파 0 통과, manual 오승격) | [docs/monitoring/2026-04-17-trading-postmortem.md](docs/monitoring/2026-04-17-trading-postmortem.md) |
| 2026-04-18 | 3 불변식 관통 (15m 단일 / canonical 5m / reconcile Pending 가드) — "수정 무한 루프" 탈출 | [docs/monitoring/2026-04-17-invariants-plan.md](docs/monitoring/2026-04-17-invariants-plan.md) |

## 운영 주의사항

### 프로세스 종료 시 (2026-04-14 사고 교훈)
1. **Ctrl+C 또는 일반 `taskkill` 사용** — graceful shutdown 경로로 30초 대기 중이던 주문을 취소 후 종료
2. **`taskkill /F` (강제 종료) 지양** — 체결 대기 중 주문이 KIS에 orphan으로 남아 증거금이 묶임
3. `taskkill /F`가 불가피한 경우 → **즉시 HTS/MTS에서 미체결 주문 수동 취소** 또는 재시작하여 자동 정리(`cancel_all_pending_orders`)
4. 실제 `cancel_all_pending_orders`의 조회 파싱이 실패하는 경우가 있음 (모의투자 환경) → **HTS 수동 확인 권장**

### WebSocket 재연결 패턴
KIS 서버가 **매 정시(:00)에 연결 리셋** (2026-04-14 확인). 자동 재연결(2~4초) 정상 동작하므로 기능 영향 없음. 로그에 "WebSocket 연결 실패" 경고가 시간당 1회 나오는 것은 정상.
