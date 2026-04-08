> [docs/README.md](../README.md) > [indicators/](./overview.md) > rsi.md

# RSI (Relative Strength Index, 상대강도지수)

## 의미와 용도

RSI는 J. Welles Wilder Jr.가 1978년에 개발한 모멘텀 지표로, 가격 변동의 **상승/하락 강도 비율**을 0~100 범위로 표현한다. 주로 **과매수/과매도** 상태를 판단하는 데 사용한다.

**핵심 질문**: "최근 가격 변동 중 상승분이 차지하는 비율은 얼마인가?"

## 계산식

### 단계별 계산

**1단계: 가격 변동 계산**

```
변동 = 당일 종가 - 전일 종가
상승분 (U) = max(변동, 0)
하락분 (D) = max(-변동, 0)
```

**2단계: 평균 상승분/하락분 계산**

첫 번째 값 (단순평균):
```
AU₁ = (U₁ + U₂ + ... + Uₙ) / n
AD₁ = (D₁ + D₂ + ... + Dₙ) / n
```

이후 값 (지수이동평균 방식, Wilder's smoothing):
```
AU(t) = (AU(t-1) × (n-1) + U(t)) / n
AD(t) = (AD(t-1) × (n-1) + D(t)) / n
```

**3단계: RS (Relative Strength) 계산**

```
RS = AU / AD
```

**4단계: RSI 계산**

```
RSI = 100 - (100 / (1 + RS))
    = 100 × AU / (AU + AD)
```

### 계산 예시 (14일 RSI)

| 일자 | 종가 | 변동 | 상승(U) | 하락(D) |
|------|------|------|---------|---------|
| 1 | 100 | - | - | - |
| 2 | 102 | +2 | 2 | 0 |
| 3 | 101 | -1 | 0 | 1 |
| 4 | 104 | +3 | 3 | 0 |
| 5 | 103 | -1 | 0 | 1 |
| 6 | 105 | +2 | 2 | 0 |
| 7 | 107 | +2 | 2 | 0 |
| 8 | 106 | -1 | 0 | 1 |
| 9 | 108 | +2 | 2 | 0 |
| 10 | 110 | +2 | 2 | 0 |
| 11 | 109 | -1 | 0 | 1 |
| 12 | 111 | +2 | 2 | 0 |
| 13 | 113 | +2 | 2 | 0 |
| 14 | 112 | -1 | 0 | 1 |
| 15 | 114 | +2 | 2 | 0 |

- AU₁ = (2+0+3+0+2+2+0+2+2+0+2+2+0+2) / 14 = 17/14 ≈ 1.214
- AD₁ = (0+1+0+1+0+0+1+0+0+1+0+0+1+0) / 14 = 4/14 ≈ 0.286
- RS = 1.214 / 0.286 ≈ 4.24
- **RSI = 100 - (100 / (1 + 4.24)) ≈ 80.9**

15일차 (증분):
- AU₂ = (1.214 × 13 + 2) / 14 ≈ 1.270
- AD₂ = (0.286 × 13 + 0) / 14 ≈ 0.265
- RSI = 100 × 1.270 / (1.270 + 0.265) ≈ **82.7**

## 해석 기준

| RSI 범위 | 의미 | 행동 |
|----------|------|------|
| **70 이상** | 과매수 (Overbought) | 매도 고려, 가격 하락 가능 |
| **50~70** | 상승 추세 | 보유 유지 |
| **50** | 중립 | 추세 판단 보류 |
| **30~50** | 하락 추세 | 관망 |
| **30 이하** | 과매도 (Oversold) | 매수 고려, 반등 가능 |

> **종목별 조정**: 변동성이 큰 종목은 80/20, 안정적인 종목은 60/40 기준 사용 가능

## 다이버전스

가격과 RSI의 움직임이 반대일 때 추세 전환을 예고한다.

### 강세 다이버전스 (Bullish Divergence)

```
가격:  ╲          ╲     ← 저점 갱신
        ╲          ╲
RSI:   ╲          ╱     ← 저점 미갱신 (높아짐)
        ╲        ╱
```

**가격은 저점을 낮추는데, RSI는 저점을 높이고 있다** → 하락세 약화, 반등 가능

### 약세 다이버전스 (Bearish Divergence)

```
가격:  ╱          ╱     ← 고점 갱신
      ╱          ╱
RSI:   ╱          ╲     ← 고점 미갱신 (낮아짐)
      ╱            ╲
```

**가격은 고점을 높이는데, RSI는 고점을 낮추고 있다** → 상승세 약화, 하락 가능

## 매매 시그널

### 기본 시그널

| 조건 | 시그널 | 강도 |
|------|--------|------|
| RSI < 30 진입 | 매수 대기 | 보통 |
| RSI가 30을 상향 돌파 | **매수** | 강함 |
| RSI > 70 진입 | 매도 대기 | 보통 |
| RSI가 70을 하향 돌파 | **매도** | 강함 |
| 강세 다이버전스 | 매수 | 강함 |
| 약세 다이버전스 | 매도 | 강함 |

### 중심선(50) 크로스

- RSI가 50을 상향 돌파 → 상승 추세 진입
- RSI가 50을 하향 돌파 → 하락 추세 진입

추세 확인용으로 단독 매매 시그널보다는 보조 지표로 활용.

## Rust 구현 가이드

```rust
pub struct Rsi {
    period: usize,
    avg_gain: Option<f64>,  // AU
    avg_loss: Option<f64>,  // AD
    prev_close: Option<f64>,
    count: usize,
    gains: Vec<f64>,        // 초기 n기간 수집용
    losses: Vec<f64>,
}

impl Rsi {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            avg_gain: None,
            avg_loss: None,
            prev_close: None,
            count: 0,
            gains: Vec::with_capacity(period),
            losses: Vec::with_capacity(period),
        }
    }

    pub fn next(&mut self, close: f64) -> Option<f64> {
        let result = if let Some(prev) = self.prev_close {
            let change = close - prev;
            let gain = if change > 0.0 { change } else { 0.0 };
            let loss = if change < 0.0 { -change } else { 0.0 };

            match (self.avg_gain, self.avg_loss) {
                (Some(ag), Some(al)) => {
                    // Wilder's smoothing
                    let new_ag = (ag * (self.period as f64 - 1.0) + gain)
                        / self.period as f64;
                    let new_al = (al * (self.period as f64 - 1.0) + loss)
                        / self.period as f64;
                    self.avg_gain = Some(new_ag);
                    self.avg_loss = Some(new_al);

                    if new_al == 0.0 {
                        Some(100.0)
                    } else {
                        Some(100.0 - (100.0 / (1.0 + new_ag / new_al)))
                    }
                }
                _ => {
                    // 초기 데이터 수집 중
                    self.gains.push(gain);
                    self.losses.push(loss);

                    if self.gains.len() == self.period {
                        let ag: f64 = self.gains.iter().sum::<f64>()
                            / self.period as f64;
                        let al: f64 = self.losses.iter().sum::<f64>()
                            / self.period as f64;
                        self.avg_gain = Some(ag);
                        self.avg_loss = Some(al);

                        if al == 0.0 {
                            Some(100.0)
                        } else {
                            Some(100.0 - (100.0 / (1.0 + ag / al)))
                        }
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        };

        self.prev_close = Some(close);
        result
    }
}
```

- **필요 최소 데이터**: n + 1개 (기본 14 + 1 = 15)
- **시간복잡도**: O(1) per update (초기화 후)
- **공간복잡도**: O(1) (초기화 후 avg_gain, avg_loss만 유지)

## 관련 문서

- [스토캐스틱](stochastic.md) — 유사한 과매수/과매도 지표
- [볼린저밴드](bollinger-bands.md) — RSI + 볼린저밴드 조합 전략
- [지표 조합 전략](strategies.md) — RSI 기반 매매 전략
