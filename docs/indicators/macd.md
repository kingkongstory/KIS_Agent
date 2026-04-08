> [docs/README.md](../README.md) > [indicators/](./overview.md) > macd.md

# MACD (Moving Average Convergence Divergence, 이동평균수렴확산)

## 의미와 용도

MACD는 Gerald Appel이 1970년대에 개발한 지표로, 두 개의 지수이동평균(EMA) 사이의 관계를 이용하여 **추세 변화와 모멘텀**을 동시에 포착한다.

**핵심 개념**: 단기 EMA와 장기 EMA의 차이가 벌어지면 모멘텀이 강하고, 줄어들면 추세가 약해지고 있다.

## 구성 요소

MACD는 세 가지 구성 요소로 이루어진다:

| 구성 요소 | 계산 | 의미 |
|-----------|------|------|
| **MACD 라인** | EMA(12) - EMA(26) | 단기/장기 추세 차이 |
| **시그널 라인** | MACD의 EMA(9) | MACD의 추세 (매매 타이밍) |
| **히스토그램** | MACD - 시그널 | 두 라인의 차이 (모멘텀 강도) |

## 계산식

### 단계별 계산

```
1. 단기 EMA 계산:  EMA(12)  ← 12일 지수이동평균 (종가 기준)
2. 장기 EMA 계산:  EMA(26)  ← 26일 지수이동평균 (종가 기준)
3. MACD 라인:      MACD = EMA(12) - EMA(26)
4. 시그널 라인:    Signal = EMA(9) of MACD values
5. 히스토그램:     Histogram = MACD - Signal
```

> EMA 계산 방법은 [이동평균선](moving-averages.md) 참조

### 계산 예시

| 일자 | 종가 | EMA(12) | EMA(26) | MACD | Signal | Histogram |
|------|------|---------|---------|------|--------|-----------|
| 26일 | 105 | 103.5 | 101.2 | +2.3 | - | - |
| 27일 | 106 | 103.9 | 101.6 | +2.3 | - | - |
| ... | ... | ... | ... | ... | ... | ... |
| 34일 | 110 | 106.2 | 103.5 | +2.7 | +2.4 | +0.3 |
| 35일 | 108 | 106.5 | 103.8 | +2.7 | +2.5 | +0.2 |
| 36일 | 106 | 106.4 | 104.0 | +2.4 | +2.5 | **-0.1** ← 모멘텀 약화 |

### 파라미터 기본값의 의미

| 파라미터 | 값 | 의미 |
|---------|-----|------|
| 단기 EMA | **12일** | 약 2.5주 (단기 추세) |
| 장기 EMA | **26일** | 약 5주, 1개월+ (장기 추세) |
| 시그널 EMA | **9일** | 약 2주 (MACD 자체의 추세) |

## 해석

### MACD 라인 위치

| 위치 | 의미 |
|------|------|
| MACD > 0 | 단기 EMA > 장기 EMA → **상승 추세** |
| MACD < 0 | 단기 EMA < 장기 EMA → **하락 추세** |
| MACD 증가 | 상승 모멘텀 강화 |
| MACD 감소 | 상승 모멘텀 약화 (또는 하락 모멘텀 강화) |

### 히스토그램

| 상태 | 의미 |
|------|------|
| 히스토그램 > 0 & 증가 | 상승 모멘텀 가속 |
| 히스토그램 > 0 & 감소 | 상승 모멘텀 감속 (주의) |
| 히스토그램 < 0 & 감소 | 하락 모멘텀 가속 |
| 히스토그램 < 0 & 증가 | 하락 모멘텀 감속 (반등 가능) |

## 매매 시그널

### 1. MACD/시그널 교차 (가장 기본)

| 조건 | 시그널 |
|------|--------|
| MACD가 시그널을 **상향 돌파** | **매수** (골든크로스) |
| MACD가 시그널을 **하향 돌파** | **매도** (데드크로스) |

```
MACD:    ──────╱──── ← MACD 위로
              ╳
Signal:  ────╱────── ← Signal 아래
         매수 시그널
```

### 2. 제로라인 교차

| 조건 | 시그널 |
|------|--------|
| MACD가 0을 **상향 돌파** | 상승 추세 확인 (매수 강화) |
| MACD가 0을 **하향 돌파** | 하락 추세 확인 (매도 강화) |

### 3. 히스토그램 반전

히스토그램의 방향 전환은 MACD/시그널 교차보다 **한 발 빨리** 추세 변화를 감지할 수 있다.

### 4. 다이버전스

RSI와 동일한 원리:
- **강세 다이버전스**: 가격 저점 갱신 + MACD 저점 미갱신 → 반등 가능
- **약세 다이버전스**: 가격 고점 갱신 + MACD 고점 미갱신 → 하락 가능

## 파라미터 조정

| 설정 | 값 | 용도 |
|------|-----|------|
| 기본 | (12, 26, 9) | 일반 스윙 트레이딩 |
| 단기 | (5, 13, 1) | 데이트레이딩, 빠른 시그널 |
| 장기 | (19, 39, 9) | 장기 추세 투자 |

> 기간을 줄이면 시그널이 빨라지지만 거짓 신호(whipsaw)가 늘어난다.

## Rust 구현 가이드

```rust
pub struct Macd {
    fast_ema: Ema,    // EMA(12)
    slow_ema: Ema,    // EMA(26)
    signal_ema: Ema,  // MACD의 EMA(9)
}

#[derive(Debug, Clone)]
pub struct MacdResult {
    pub macd: f64,       // MACD 라인
    pub signal: f64,     // 시그널 라인
    pub histogram: f64,  // 히스토그램
}

impl Macd {
    pub fn new(fast: usize, slow: usize, signal: usize) -> Self {
        Self {
            fast_ema: Ema::new(fast),
            slow_ema: Ema::new(slow),
            signal_ema: Ema::new(signal),
        }
    }

    /// 기본 설정 (12, 26, 9)
    pub fn default() -> Self {
        Self::new(12, 26, 9)
    }

    pub fn next(&mut self, close: f64) -> Option<MacdResult> {
        let fast = self.fast_ema.next(close)?;
        let slow = self.slow_ema.next(close)?;
        let macd_value = fast - slow;
        let signal = self.signal_ema.next(macd_value)?;
        let histogram = macd_value - signal;

        Some(MacdResult {
            macd: macd_value,
            signal,
            histogram,
        })
    }
}
```

- **필요 최소 데이터**: slow + signal - 1 = 26 + 9 - 1 = **34개**
- **시간복잡도**: O(1) per update (EMA 3개)
- **공간복잡도**: O(1) (EMA 초기화 후)
- **의존 모듈**: [Ema](moving-averages.md) 재사용

## 관련 문서

- [이동평균선](moving-averages.md) — EMA 계산의 기반
- [RSI](rsi.md) — 모멘텀 지표 비교
- [지표 조합 전략](strategies.md) — MA + MACD 추세추종 전략
