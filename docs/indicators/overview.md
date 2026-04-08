> [docs/README.md](../README.md) > indicators/ > overview.md

# 기술적 분석 개요

## KIS API와 기술적 지표

KIS Open Trading API는 기술적 지표를 직접 제공하지 않는다. 대신 [기간별 시세 API](../api/domestic-stock-quotations.md)에서 **OHLCV 데이터**를 제공하며, 이 데이터를 기반으로 지표를 직접 계산해야 한다.

```
KIS API (OHLCV) → 지표 엔진 (계산) → 매매 시그널 → 주문 실행
```

## OHLCV 데이터란

하나의 캔들(봉)은 특정 기간의 가격 정보를 담고 있다:

| 요소 | 영문 | 설명 |
|------|------|------|
| 시가 | Open | 기간 시작 시점의 가격 |
| 고가 | High | 기간 중 최고가 |
| 저가 | Low | 기간 중 최저가 |
| 종가 | Close | 기간 마감 시점의 가격 |
| 거래량 | Volume | 기간 중 거래된 주식 수 |

## 캔들차트 기초

### 양봉과 음봉

```
  양봉 (상승)        음봉 (하락)
  │                  │
  ├─┐ ← 고가         ├─┐ ← 고가
  │ │                │█│
  │ │ ← 종가 (위)    │█│ ← 시가 (위)
  │ │                │█│
  │ │ ← 시가 (아래)  │█│ ← 종가 (아래)
  │ │                │█│
  ├─┘ ← 저가         ├─┘ ← 저가
  │                  │
```

- **양봉**: 종가 > 시가 (상승 마감)
- **음봉**: 종가 < 시가 (하락 마감)
- **윗꼬리**: 고가와 몸통 상단 사이 — 길수록 매도 압력
- **아랫꼬리**: 저가와 몸통 하단 사이 — 길수록 매수 지지

### 주요 캔들 패턴

| 패턴 | 형태 | 의미 |
|------|------|------|
| **도지 (Doji)** | 시가 ≈ 종가 (십자형) | 매수/매도 균형, 추세 전환 가능 |
| **망치형 (Hammer)** | 짧은 몸통 + 긴 아랫꼬리 | 하락 후 반등 시그널 |
| **교수형 (Hanging Man)** | 망치형과 동일 형태 (상승 중) | 상승 후 하락 전환 가능 |
| **장악형 (Engulfing)** | 전일 봉을 완전히 감싸는 봉 | 강한 추세 전환 |
| **샛별형 (Morning Star)** | 큰 음봉 + 작은 봉 + 큰 양봉 | 바닥 반전 시그널 |
| **석별형 (Evening Star)** | 큰 양봉 + 작은 봉 + 큰 음봉 | 천정 반전 시그널 |

## 기술적 지표 분류

### 추세 지표 (Trend Indicators)

현재 시장의 방향성을 판단한다.

| 지표 | 핵심 | 문서 |
|------|------|------|
| 이동평균선 (MA) | 평균 가격으로 추세 방향 파악 | [moving-averages.md](moving-averages.md) |
| MACD | 두 EMA 차이로 추세 변화 감지 | [macd.md](macd.md) |

### 모멘텀 지표 (Momentum Indicators)

가격 변동의 속도와 강도를 측정한다.

| 지표 | 핵심 | 문서 |
|------|------|------|
| RSI | 상승/하락 강도 비율 (0~100) | [rsi.md](rsi.md) |
| 스토캐스틱 | 가격 범위 내 위치 (0~100) | [stochastic.md](stochastic.md) |

### 변동성 지표 (Volatility Indicators)

가격 변동의 폭을 측정한다.

| 지표 | 핵심 | 문서 |
|------|------|------|
| 볼린저밴드 | 이동평균 ± 표준편차 밴드 | [bollinger-bands.md](bollinger-bands.md) |

### 거래량 지표 (Volume Indicators)

가격 움직임의 신뢰도를 거래량으로 확인한다.

| 지표 | 핵심 | 문서 |
|------|------|------|
| OBV | 거래량 누적 흐름 | [volume-indicators.md](volume-indicators.md) |
| VWAP | 거래량 가중 평균가 | [volume-indicators.md](volume-indicators.md) |

## 지표 엔진 아키텍처

### Indicator trait

모든 지표가 구현할 공통 인터페이스:

```rust
/// 기술적 지표 공통 인터페이스
pub trait Indicator {
    /// 지표의 이름
    fn name(&self) -> &str;

    /// 지표 계산에 필요한 최소 캔들 수
    fn required_candles(&self) -> usize;

    /// 캔들 배열로부터 지표값 계산 (배치)
    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError>;

    /// 새로운 캔들 하나 추가로 지표값 증분 업데이트 (스트리밍)
    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError>;
}

/// 지표 계산 결과
#[derive(Debug, Clone)]
pub struct IndicatorValue {
    pub date: NaiveDate,
    pub values: HashMap<String, f64>,  // "rsi" -> 65.3, "upper" -> 75000.0 등
    pub signal: Signal,
}

/// 매매 시그널
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    StrongBuy,   // 강한 매수
    Buy,         // 매수
    Hold,        // 관망
    Sell,        // 매도
    StrongSell,  // 강한 매도
}
```

### 배치 vs 스트리밍 계산

| 방식 | 용도 | 설명 |
|------|------|------|
| 배치 | 과거 데이터 분석, 백테스트 | `calculate(&[Candle])` — 전체 데이터를 한 번에 계산 |
| 스트리밍 | 실시간 데이터 반영 | `update(&Candle)` — 새 캔들 하나로 증분 업데이트 |

배치 계산은 정확하지만 전체 재계산이 필요하고, 스트리밍은 효율적이지만 상태 관리가 필요하다. 두 방식 모두 지원해야 한다.

### 지표 파이프라인

```
OHLCV 데이터 수집
  ↓
캔들 배열 구성
  ↓
┌─────────────────────┐
│  지표 계산 (병렬)    │
│  ├ MA(5, 20, 60)    │
│  ├ RSI(14)          │
│  ├ MACD(12,26,9)    │
│  ├ 볼린저(20,2)     │
│  └ OBV              │
└─────────────────────┘
  ↓
시그널 종합 (strategies.md)
  ↓
매매 결정 → 주문 실행
```

## 관련 문서

- [국내주식 시세](../api/domestic-stock-quotations.md) — OHLCV 데이터 소스
- [WebSocket](../api/websocket.md) — 실시간 데이터 수신
- [지표 조합 전략](strategies.md) — 시그널 종합 및 매매 전략
