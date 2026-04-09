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

## 자동매매 시스템

### ORB+FVG 전략 흐름

1. **OR 수집** (09:00~09:15): Opening Range 고가/저가 산출
2. **FVG 탐색** (09:15~15:20): 5분봉에서 3캔들 갭 패턴 탐지 + OR 돌파 확인
3. **진입**: FVG 리트레이스 시 시장가 매수
4. **청산**: TP 지정가 매도 (KRX 자동 체결) / SL·트레일링·시간스탑은 시장가

### 주문 방식 (하이브리드)

| 동작 | 주문 유형 | 이유 |
|---|---|---|
| 진입 (매수) | 시장가 (`ORD_DVSN=01`) | 신호 발생 시 즉시 체결 보장 |
| 익절 (TP) | 지정가 (`ORD_DVSN=00`) | 정확한 목표가 체결, 슬리피지 0 |
| 손절 (SL) | 시장가 | 즉시 청산 (TP 취소 후 실행) |
| 장마감 청산 | 시장가 | TP 취소 후 강제 청산 |

### 데이터 소스 분리

- **OR 범위 계산**: 네이버 백필 + DB 저장 데이터 포함 전체 캔들
- **FVG 신호 탐색**: WebSocket 실시간 수집 분봉만 (`is_realtime=true`)
- 네이버 백필 데이터는 종가만 제공(OHLC 동일) → FVG 탐색에 부적합

### 서버 재시작 복구

- **OR 범위**: `daily_or_range` 테이블에서 당일 OR 로드
- **활성 포지션**: `active_positions` 테이블에서 포지션 + TP 주문번호 복구
- **이전 TP 지정가**: 저장된 주문번호로 취소 → 새 TP 재발주
- **분봉 데이터**: `minute_ohlcv`에서 당일 분봉 프리로딩
- OR 데이터 없으면 네이버 금융에서 자동 백필

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

### 전략 파라미터 (OrbFvgConfig)

- RR 비율: 2.0 (손익비 1:2)
- OR: 09:00~09:15
- 진입 마감: 15:20, 강제 청산: 15:25
- 트레일링: 0.5R, 본전스탑: 1.0R
- 일일 최대 손실: -1.5% (거래 횟수 제한 없음)
