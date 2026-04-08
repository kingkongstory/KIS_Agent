> [docs/README.md](../README.md) > [api/](./overview.md) > github-resources.md

# KIS GitHub 리소스

**GitHub 조직**: https://github.com/koreainvestment

## 레포지토리 목록

### 1. open-trading-api (⭐1.2k)

**URL**: https://github.com/koreainvestment/open-trading-api

KIS Open API의 공식 샘플 코드 및 전략 도구 모음.

#### 디렉토리 구조

```
open-trading-api/
├── examples_llm/          # LLM용 단일 함수 예제
│   ├── auth/              # 인증
│   ├── domestic_stock/    # 국내주식
│   ├── overseas_stock/    # 해외주식
│   └── ...
├── examples_user/         # 사용자용 카테고리별 통합 예제
│   ├── domestic_stock/    # 국내주식 (함수+예제)
│   ├── overseas_stock/    # 해외주식
│   └── websocket/         # WebSocket 예제
├── strategy_builder/      # 전략 빌더 (80+ 기술지표)
│   ├── core/
│   │   └── indicators.py  # 96개 기술지표 계산 모듈
│   ├── strategy_core/
│   │   └── preset/        # 10개 프리셋 전략
│   ├── backend/           # FastAPI 서버 (:8000)
│   └── frontend/          # Next.js UI (:3000)
├── backtester/            # QuantConnect Lean 백테스터 (Docker)
├── MCP/                   # AI 도구 연결 레이어
└── kis_auth.py            # 인증 모듈
```

#### 우리 프로젝트에 활용할 수 있는 것

| 리소스 | 경로 | 활용 |
|--------|------|------|
| **기술지표 계산 로직** | `strategy_builder/core/indicators.py` | Rust 포팅 시 참조 (96개 지표의 Python 구현체) |
| **프리셋 전략** | `strategy_builder/strategy_core/preset/` | 전략 설계 참조 |
| **API 호출 예제** | `examples_llm/domestic_stock/` | 요청/응답 형식 확인 |
| **WebSocket 예제** | `examples_user/websocket/` | 연결/파싱 로직 참조 |
| **인증 흐름** | `kis_auth.py` | 토큰 관리 패턴 참조 |

### 2. kis-ai-extensions (⭐120)

**URL**: https://github.com/koreainvestment/kis-ai-extensions

AI 에이전트에서 KIS API를 활용하기 위한 확장 기능 모음.

#### 핵심 기능

| 스킬 | 트리거 | 역할 |
|------|--------|------|
| `kis-strategy-builder` | "전략 만들어줘" | 80+ 지표 조합으로 `.kis.yaml` 전략 생성 |
| `kis-backtester` | "백테스트 해줘" | Docker 기반 백테스트, HTML 리포트 |
| `kis-order-executor` | "신호 확인해줘" | BUY/SELL/HOLD 신호 → 주문 실행 |
| `kis-team` | "다 해줘" | 전략→백테스트→주문 풀 파이프라인 |
| `kis-cs` | 오류 발생 시 | 에러 해석 및 고객 서비스 |

#### MCP 서버

```json
{
  "mcpServers": {
    "kis-backtest": {
      "type": "http",
      "url": "http://127.0.0.1:3846/mcp"
    }
  }
}
```

**MCP 도구**:
- `run_backtest` — 전략 백테스트 실행
- `optimize_params` — Grid/Random 파라미터 최적화
- `get_report` — HTML 리포트 조회
- `list_strategies` — 가용 전략 목록

#### 설치

```bash
npx @koreainvestment/kis-quant-plugin init --agent claude
```

#### 지원 AI 에이전트

Claude Code, Cursor, Codex, Gemini CLI

#### 보안

- 자격증명 하드코딩 차단 (PreToolUse 훅)
- 모든 거래 활동 로깅
- 실전 주문 시 사용자 승인 강제
- 신호 강도 0.5 미만 자동 건너뜀

## .kis.yaml 전략 형식

strategy_builder와 backtester가 공유하는 전략 정의 형식:

```yaml
name: "골든크로스"
description: "단기 MA가 장기 MA를 상향 돌파 시 매수"
category: "trend"

indicators:
  - name: SMA
    alias: sma_fast
    params:
      period: 5
      source: close
  - name: SMA
    alias: sma_slow
    params:
      period: 20
      source: close

entry:
  logic: AND
  conditions:
    - left: sma_fast
      op: cross_above
      right: sma_slow

exit:
  logic: OR
  conditions:
    - left: sma_fast
      op: cross_below
      right: sma_slow

risk:
  stop_loss:
    enabled: true
    pct: 5.0
  take_profit:
    enabled: false
    pct: 10.0
  trailing_stop:
    enabled: false
    pct: 3.0
```

> 이 형식은 우리 Rust 프로젝트에서 전략 설정 파일로 채택할 수 있다.

## 관련 문서

- [기술지표 카탈로그](../indicators/technical-indicators-catalog.md) — 96개 지표 전체 목록
- [지표 조합 전략](../indicators/strategies.md) — 10개 프리셋 전략 상세
- [API 공통사항](overview.md) — KIS API 기본 정보
