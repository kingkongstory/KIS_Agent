# ORB+FVG 전략 문서

## 전략 개요

Opening Range Breakout(ORB) + Fair Value Gap(FVG) 기반 장 초반 모멘텀 매매 전략.
KIS 모의투자 계좌에서 KODEX 레버리지(122630) + KODEX 인버스(114800) 2종목을 동시 운영한다.

## 전략 규칙

### 1단계: OR 범위 확정 (09:00~09:15)

- 15분봉 기준 장 시작 첫 15분간의 **고가(OR HIGH)**와 **저가(OR LOW)** 확정
- 5분봉 3개 캔들(09:00, 09:05, 09:10)의 max(high) / min(low)

### 2단계: 5분봉 전환 → 돌파 + FVG 동시 감지 (09:15~)

3개 연속 5분봉 캔들(A, B, C)을 검사:

**Bullish (매수):**
- B캔들이 양봉(close > open)
- B캔들 close > OR HIGH (몸통 돌파)
- A.high < C.low (FVG 갭 형성)
- FVG 영역 = [A.high, C.low]

**Bearish (매도):**
- B캔들이 음봉(close < open)
- B캔들 close < OR LOW (몸통 돌파)
- A.low > C.high (FVG 갭 형성)
- FVG 영역 = [C.high, A.low]

### 3단계: FVG 리트레이스 → 진입

이후 캔들의 저가(Long) 또는 고가(Short)가 FVG 영역에 진입하면 매수/매도.
- 진입가 = FVG 중간값
- 손절 = A캔들 저점(Long) / A캔들 고점(Short)
- 익절 = 진입가 + 리스크 × RR배수

### FVG 유효시간

FVG 형성 후 **4캔들(20분)** 이내 리트레이스가 없으면 폐기하고 새 신호를 재탐색한다.

## 청산 관리

| 순서 | 조건 | 동작 |
|------|------|------|
| 1 | 1R 도달 | 손절가를 진입가로 이동 (본전 스탑) |
| 2 | 1R 이후 추가 상승 | 0.5R 간격으로 손절가 추적 (트레일링 스탑) |
| 3 | 30분(6캔들) 경과 + 1R 미달 | 수익/손실 무관 현재가 청산 (시간 스탑) |
| 4 | 15:25 | 보유 포지션 강제 청산 (장마감) |

## 2차 진입 규칙

- 1차 거래 **수익 + 수익률 0.5% 이상** → 같은 방향 FVG만으로 2차 진입 허용 (OR 돌파 조건 생략)
- 1차 거래 **손실** → 하루 종료, 추가 진입 없음
- 하루 **최대 2회** 진입

## 백테스트 결과 (60일, 2026-01-09 ~ 2026-04-08)

### KODEX 레버리지 (122630)

| 항목 | 값 |
|------|-----|
| 테스트 기간 | 59거래일 |
| 총 거래 | 58회 |
| 승률 | **91.4%** (53승 5패) |
| 총 손익 | **+33.11%** |
| 평균 R배수 | 0.80R |
| 일평균 수익 | **+0.56%/일** |

### KODEX 인버스 (114800)

| 항목 | 값 |
|------|-----|
| 테스트 기간 | 59거래일 |
| 총 거래 | 57회 |
| 승률 | **91.2%** (52승 5패) |
| 총 손익 | **+18.81%** |
| 평균 R배수 | 0.87R |
| 일평균 수익 | **+0.32%/일** |

### 2종목 합산

| 항목 | 값 |
|------|-----|
| 총 거래 | 115회 |
| **총 손익** | **+51.92%** |
| **일평균** | **+0.88%/일** |

### 월별 추이

| 월 | 레버리지 | 인버스 | 합산 |
|---|---|---|---|
| 1월 | +6.83% | +0.12% | +6.95% |
| 2월 | -1.08% | -1.70% | -2.78% |
| 3월 | +5.52% | +4.39% | +9.91% |
| 4월 (8일) | +1.94% | +1.79% | +3.73% |

### 청산 유형 분석 (레버리지 기준)

| 청산유형 | 건수 | 평균 손익 |
|---------|------|----------|
| 트레일링 스탑 | 15건 | +0.48% |
| 시간 스탑 | 7건 | +0.16% |
| 손절 (수익 방향) | 13건 | +0.62% |
| 손절 (손실 방향) | 14건 | -0.72% |
| 익절 (TP) | 1건 | +3.59% |
| 장마감 | 1건 | +0.20% |

## 설정 파라미터 (최적화 완료)

| 파라미터 | 기본값 | 설명 |
|---------|--------|------|
| `rr_ratio` | 2.0 | 손익비 (1:2) |
| `or_start` | 09:00 | OR 시작 시각 |
| `or_end` | 09:15 | OR 종료 / 스캐닝 시작 |
| `entry_cutoff` | 15:20 | 신규 진입 마감 |
| `force_exit` | 15:25 | 강제 청산 시각 |
| `time_stop_candles` | **3** | 시간 스탑 캔들 수 (**15분**) |
| `trailing_r` | **0.1** | 트레일링 간격 (**0.1R**, 타이트) |
| `breakeven_r` | **0.3** | 본전 스탑 활성화 (**0.3R**에 조기 보호) |
| `fvg_expiry_candles` | **6** | FVG 유효시간 (**30분**) |
| `min_first_pnl_for_second` | **0.0** | 2차 진입 (수익이면 무조건 허용) |

### 파라미터 최적화 근거

60일 백테스트 기반 체계적 스윕 결과:
- `trailing_r` 0.1이 0.5 대비 **+90% 수익** (타이트할수록 소폭 수익을 빠르게 확보)
- `breakeven_r` 0.3이 1.0 대비 **+55% 수익** (0.3R에서 조기 보호 → 손실 전환 방지)
- `time_stop_candles` 3이 6 대비 **+5% 수익** (15분 내 판단, 빠른 정리)
- `fvg_expiry_candles` 6이 4 대비 **+12% 수익** (FVG에 더 넓은 시간 허용)
- `min_first_pnl_for_second` 0.0이 0.5 대비 **+9% 수익** (2차 진입 기회 최대화)

## 웹 대시보드

```bash
cargo run -- server    # 백엔드: http://127.0.0.1:3000
cd frontend && npm run dev  # 프론트: http://localhost:5173
```

### 화면 구성
- **ORB+FVG 자동매매 패널**: 레버리지/인버스 각각 토글 온오프, 상태 표시
- **가격 카드**: 두 종목 현재가, 등락률, 시고저, 거래대금 (5초 갱신)
- **캔들 차트**: 두 종목 일봉 차트 나란히 표시

### API 엔드포인트
| 메서드 | 경로 | 설명 |
|--------|------|------|
| GET | `/api/v1/strategy/status` | 전략 실행 상태 조회 |
| POST | `/api/v1/strategy/start` | 자동매매 시작 `{"code":"122630"}` |
| POST | `/api/v1/strategy/stop` | 자동매매 중지 `{"code":"122630"}` |
| GET | `/api/v1/stocks/{code}/price` | 현재가 조회 |
| GET | `/api/v1/stocks/{code}/candles` | 캔들 데이터 조회 |

## CLI 사용법

```bash
# 백테스트 (기본 최적화 파라미터 적용)
cargo run -- backtest 122630 --days 60
cargo run -- backtest 114800 --days 60

# 백테스트 파라미터 커스터마이징
cargo run -- backtest 122630 --days 60 --trail 0.1 --be-r 0.3 --tstop 3 --fvg-exp 6 --min2nd 0.0

# 라이브 운영과 동일한 모드 (Multi-Stage + dual_locked)
cargo run -- backtest 122630 --days 1 --rr 2.5 --multi-stage --dual-locked

# 실시간 모의투자
cargo run -- trade 122630 --qty 1 --rr 2.0

# Yahoo Finance 5분봉 수집 (60d = 약 59 영업일치, 매일 실행)
cargo run -- collect-yahoo --codes "122630,114800" --interval 5m --range 60d
```

## 데이터 소스

| 소스 | 용도 | 범위 | 비고 |
|------|------|------|------|
| **Yahoo Finance** | **백테스트 5분봉 (메인)** | **60d (~59 영업일)** | **진짜 OHLC, KST timezone 정확. 60d가 yahoo 절대 천장 (90d/6mo/1y 거부). 매일 60d 호출로 누적** |
| 네이버 금융 일봉 | 일봉 OHLCV / KOSPI·KOSDAQ 지수 | 수 년치 | 일봉만 사용. 분봉은 close-only라 미사용 |
| KIS WebSocket | 실시간 시세 (라이브 운영) | 라이브 | 매매 엔진용 |
| LS증권 Open API | 1년치 정밀 분봉 (예비) | 1년 | 계좌/키 미발급 상태 |
| PostgreSQL | 수집 데이터 저장 | 누적 | `minute_ohlcv` 테이블 (5분봉만 사용) |

## 일일 운영 절차

매 거래일 장 마감 후 **당일 5분봉만** Yahoo에서 받아 DB에 추가한다 (`range=1d` → 73 캔들/종목). UPSERT라 안전, **DB는 매일 1일치씩 영구 누적**되며 60일 윈도우 슬라이딩이 아니다.

```bash
# 1. 장 마감 후 (15:30 이후) — Yahoo 5분봉 당일 추가
./target/release/kis_agent.exe collect-yahoo --codes "122630,114800" --interval 5m --range 1d

# 2. 당일 결과 재현 백테스트 (라이브와 동일한 Multi-Stage + dual_locked 모드)
./target/release/kis_agent.exe backtest 122630 --days 1 --rr 2.5 --multi-stage --dual-locked

# 3. 누적 백테스트 (DB가 보유한 모든 일자, 시간이 지날수록 길어짐)
./target/release/kis_agent.exe backtest 122630 --days 365 --rr 2.5 --multi-stage --dual-locked
```

### 자동화 (Windows Task Scheduler)

평일 15:35에 자동 실행하도록 등록:

```cmd
schtasks /Create /SC WEEKLY /D MON,TUE,WED,THU,FRI ^
  /TN "KIS_Yahoo_Daily" ^
  /TR "C:\workspace_google\KIS_Agent\target\release\kis_agent.exe collect-yahoo --codes 122630,114800 --interval 5m --range 1d" ^
  /ST 15:35
```

확인/삭제:
```cmd
schtasks /Query /TN "KIS_Yahoo_Daily"
schtasks /Delete /TN "KIS_Yahoo_Daily" /F
```

휴장일에는 yahoo가 빈 응답을 주므로 별도 휴장 체크 로직 불필요.

### 최초 적재 (한 번만)

빈 DB부터 시작할 때 60일치 한 번에 받아 베이스 라인 만든 뒤 다음 날부터 일일 1d 호출로 전환:

```bash
./target/release/kis_agent.exe collect-yahoo --codes "122630,114800" --interval 5m --range 60d
```

현재 DB에는 이미 2026-01-13 이후 60일치(122630/114800 각 4239건)가 있음.

### 데이터 소스 결정 근거 (2026-04-10)

네이버 분봉(`collect-minute-naver`)은 1분 종가만 제공하여 5분봉 OHLC를 만들면 H/L이 1분 close에서만 결정된다. 1분 내 wick(특히 09:00 동시호가)을 못 잡아 OR 범위가 좁아지고 가짜 진입 신호가 발생한다. 04-10 검증: 네이버 데이터로 5건 백테스트 → Yahoo로 1건. 차이 4건은 모두 가짜 wick이 만든 가짜 신호.

Yahoo Finance는 진짜 OHLC + 거래량을 제공하며 `exchangeTz=Asia/Seoul, gmtoffset=32400` 명시. 라이브 모니터링 보고서의 OR과 비교 시 114800은 완전 일치, 122630은 Yahoo가 5~15원 더 넓음 (라이브가 잡지 못한 미세 wick까지 포함된 정확한 ground truth).

**Yahoo 5분봉의 한계**: 단일 호출 최대 `range=60d` (90d/6mo/1y는 yahoo가 명시 거부, `max`는 일봉 fall-back). 즉 과거로 시간 여행 불가. 매일 1d 호출로 누적해야 시간이 지남에 따라 백테스트 가능 기간이 길어진다. 향후 1년치 일괄 적재가 필요하면 LS증권 Open API(계좌 미발급 상태)가 옵션이다.

> **timezone 주의**: `PostgresStore::new`는 모든 sqlx 연결에 `SET TIME ZONE 'Asia/Seoul'`을 강제한다 (2026-04-10 추가). 이게 빠지면 KST 시각으로 저장된 `timestamptz` 데이터를 NaiveDateTime으로 조회할 때 0건이 반환된다. 사고 분석은 [docs/monitoring/2026-04-10.md](../monitoring/2026-04-10.md) 참조.

## 주요 발견 사항

1. **익절(TP)에 도달하는 거래가 거의 없다** — RR 설정(1:2/1:3)은 실질적으로 무의미. 트레일링 스탑이 수익의 핵심.
2. **시간 스탑은 수익/손실 모두에 적용해야 한다** — 30분 내 움직이지 않으면 모멘텀이 소진된 것.
3. **FVG 유효시간 제한이 중요** — 75분 뒤 늦은 진입은 모멘텀 소진 후 진입이 되어 대부분 손실.
4. **2차 진입은 1차 수익 0.5% 이상일 때만** — 약한 1차 수익 후 2차 진입은 고점 추격 위험.
5. **09:00 캔들은 동시호가 영향** — 한국 시장 구조적 현상, 모든 데이터 소스 공통.
6. **2월(일방향 급등장)이 유일한 마이너스** — 변동성이 아니라 추세 방향의 문제.
