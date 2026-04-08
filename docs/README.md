# KIS_Agent 문서

KIS_Agent 프로젝트의 개발 참조 문서이다. 한국투자증권 Open Trading API 사용법과 기술적 분석 지표를 정리한다.

> **CLAUDE.md**는 프로젝트 지침서, **docs/**는 상세 참조 문서 역할을 한다.

## 문서 목차

### API 레퍼런스

| 문서 | 설명 |
|------|------|
| [API 공통사항](api/overview.md) | 기본 URL, 공통 헤더, tr_id 체계, 공통 응답 형식 |
| [인증](api/authentication.md) | 토큰 발급, WebSocket 접속키, 토큰 생명주기 관리 |
| [국내주식 시세](api/domestic-stock-quotations.md) | 현재가, 호가, 기간별 시세(OHLCV) 조회 |
| [국내주식 주문](api/domestic-stock-trading.md) | 매수, 매도, 정정, 취소 |
| [계좌 조회](api/account.md) | 잔고, 일별 체결내역, 매수가능액 |
| [WebSocket 실시간](api/websocket.md) | 실시간 체결가, 호가, 체결통보 |
| [속도 제한](api/rate-limits.md) | 호출 제한 규칙 및 대응 전략 |
| [에러 처리](api/error-handling.md) | 에러 코드 체계 및 Rust 에러 타입 설계 |
| [데이터 타입](api/data-types.md) | KIS JSON → Rust 타입 매핑 전략 |
| [KIS GitHub 리소스](api/github-resources.md) | 공식 레포지토리, Strategy Builder, AI Extensions |

### 기술적 지표

| 문서 | 설명 |
|------|------|
| [기술적 분석 개요](indicators/overview.md) | 지표 계산 방식, 캔들차트 기초, 지표 엔진 아키텍처 |
| [이동평균선](indicators/moving-averages.md) | SMA, EMA, WMA — 추세 판단의 기본 |
| [RSI](indicators/rsi.md) | 상대강도지수 — 과매수/과매도 판단 |
| [MACD](indicators/macd.md) | 이동평균 수렴확산 — 추세 전환 포착 |
| [볼린저밴드](indicators/bollinger-bands.md) | 변동성 기반 동적 지지/저항선 |
| [스토캐스틱](indicators/stochastic.md) | 가격 위치 기반 오실레이터 |
| [거래량 지표](indicators/volume-indicators.md) | OBV, VWAP — 거래량 기반 분석 |
| [지표 조합 전략](indicators/strategies.md) | KIS 프리셋 10개, 다중 지표 조합, 시그널 엔진 |
| [기술지표 카탈로그](indicators/technical-indicators-catalog.md) | **96개 전체 지표 목록**, 우선순위별 분류 |

## 개발 순서 권장사항

아래 순서는 모듈 간 의존성을 반영한 구현 순서이다:

```
1. 인증 (TokenManager)
   ↓
2. 시세 조회 (현재가, 호가, OHLCV)
   ↓
3. 주문 (매수/매도/정정/취소)
   ↓
4. 계좌 (잔고, 체결내역)
   ↓
5. WebSocket (실시간 시세)
   ↓
6. 지표 엔진 (MA, RSI, MACD, BB 등)
   ↓
7. 전략 엔진 (시그널 생성 → 주문 실행)
```

## 외부 리소스

- [KIS Developers 포탈](https://apiportal.koreainvestment.com)
- [KIS GitHub 조직](https://github.com/koreainvestment) — 공식 레포지토리 (3개)
  - [open-trading-api](https://github.com/koreainvestment/open-trading-api) — API 샘플, Strategy Builder (96개 지표), 백테스터
  - [kis-ai-extensions](https://github.com/koreainvestment/kis-ai-extensions) — AI 에이전트 확장 (MCP 서버, 스킬)
- [KIS API 샘플 코드](https://apiportal.koreainvestment.com/apiservice/apiservice-sample)
