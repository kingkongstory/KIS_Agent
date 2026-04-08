> [docs/README.md](../README.md) > [indicators/](./overview.md) > bollinger-bands.md

# 볼린저밴드 (Bollinger Bands)

## 의미와 용도

볼린저밴드는 John Bollinger가 1980년대에 개발한 변동성 지표로, 이동평균선을 중심으로 **표준편차 기반의 상/하단 밴드**를 형성한다. 가격의 상대적 높낮이를 판단하고, 변동성 변화를 감지하는 데 사용한다.

**핵심 개념**: 가격은 대부분(약 95%) 볼린저밴드 안에서 움직인다. 밴드를 벗어나면 비정상적 상황이다.

## 구성 요소

```
─── 상단밴드 (Upper Band) ───────────── SMA + 2σ
         ╲                    ╱
          ╲    가격 영역     ╱
           ╲               ╱
─── 중심선 (Middle Band) ──────────── SMA(20)
           ╱               ╲
          ╱    가격 영역     ╲
         ╱                    ╲
─── 하단밴드 (Lower Band) ───────────── SMA - 2σ
```

| 구성 요소 | 계산 | 의미 |
|-----------|------|------|
| **중심선** | SMA(20) | 20일 단순이동평균 |
| **상단밴드** | SMA(20) + 2σ | 상방 가격 한계 |
| **하단밴드** | SMA(20) - 2σ | 하방 가격 한계 |

## 계산식

### 단계별 계산

**1단계: SMA(n) 계산** ([이동평균선](moving-averages.md) 참조)

```
SMA(n) = (C₁ + C₂ + ... + Cₙ) / n
```

**2단계: 표준편차(σ) 계산**

```
σ = √(Σ(Cᵢ - SMA)² / n)
```

**3단계: 밴드 계산**

```
Upper = SMA(n) + k × σ
Lower = SMA(n) - k × σ
```

기본값: n = 20, k = 2

### 보조 지표

**밴드폭 (Bandwidth)**
```
Bandwidth = (Upper - Lower) / Middle × 100
```
변동성의 크기를 정량화. 밴드폭이 좁으면 변동성이 낮다.

**%B (%Bollinger)**
```
%B = (현재가 - Lower) / (Upper - Lower)
```
현재가가 밴드 내에서 어디에 위치하는지 0~1 범위로 표현.

| %B 값 | 위치 |
|--------|------|
| > 1.0 | 상단밴드 위 (상단 돌파) |
| 1.0 | 상단밴드 |
| 0.5 | 중심선 |
| 0.0 | 하단밴드 |
| < 0.0 | 하단밴드 아래 (하단 이탈) |

### 계산 예시 (20일, k=2)

20일간 종가 합 = 2,000, 평균 = 100, 표준편차 = 5

```
중심선 = 100
상단밴드 = 100 + 2 × 5 = 110
하단밴드 = 100 - 2 × 5 = 90
밴드폭 = (110 - 90) / 100 × 100 = 20%
현재가 105일 때: %B = (105 - 90) / (110 - 90) = 0.75
```

## 해석

### 볼린저 스퀴즈 (Squeeze)

밴드폭이 좁아지는 현상 → **큰 가격 변동 예고**

```
      ╲               ╱         밴드 수축 후 확장
       ╲─────────────╱
        ═════════════     ← 스퀴즈 구간
       ╱─────────────╲
      ╱               ╲
```

- 밴드폭이 6개월 최저 수준으로 줄어들면 스퀴즈 상태
- 방향은 예측 불가 → 돌파 방향 확인 후 진입

### 볼린저 바운스 (Bounce)

가격이 밴드에 닿으면 중심선으로 회귀하는 경향:

| 상황 | 해석 |
|------|------|
| 가격이 하단밴드에 닿음 | 과매도, 중심선으로 반등 가능 |
| 가격이 상단밴드에 닿음 | 과매수, 중심선으로 하락 가능 |
| 가격이 중심선에서 반등 | 추세 내 조정 완료, 추세 재개 |

> **주의**: 강한 추세에서는 가격이 밴드를 따라 이동("밴드 워킹")할 수 있다. 밴드 접촉이 항상 반전을 의미하지는 않는다.

### 밴드 워킹 (Band Walking)

강한 상승/하락 추세에서 가격이 상단/하단 밴드를 따라 이동하는 현상:

- 상단 밴드 워킹 = 강한 상승 추세 유지 (매도하지 않음)
- 하단 밴드 워킹 = 강한 하락 추세 유지 (매수하지 않음)

## 매매 시그널

### 기본 시그널

| 조건 | 시그널 | 주의 |
|------|--------|------|
| 가격이 하단밴드 접촉 + RSI < 30 | **강한 매수** | 반드시 RSI 확인 |
| 가격이 상단밴드 접촉 + RSI > 70 | **강한 매도** | 반드시 RSI 확인 |
| 스퀴즈 후 상방 돌파 + 거래량 증가 | 매수 | 돌파 확인 필수 |
| 스퀴즈 후 하방 돌파 + 거래량 증가 | 매도 | 돌파 확인 필수 |

### %B 기반 매매

| %B 범위 | 해석 | 행동 |
|---------|------|------|
| %B < 0 | 하단 이탈 (극단적 과매도) | 매수 대기 (바닥 확인 후) |
| 0 < %B < 0.2 | 하단 근처 | 매수 고려 |
| 0.8 < %B < 1.0 | 상단 근처 | 매도 고려 |
| %B > 1 | 상단 돌파 (극단적 과매수) | 매도 대기 (천정 확인 후) |

## Rust 구현 가이드

```rust
pub struct BollingerBands {
    period: usize,
    multiplier: f64,    // k (기본 2.0)
    buffer: VecDeque<f64>,
}

#[derive(Debug, Clone)]
pub struct BollingerResult {
    pub upper: f64,       // 상단밴드
    pub middle: f64,      // 중심선 (SMA)
    pub lower: f64,       // 하단밴드
    pub bandwidth: f64,   // 밴드폭 (%)
    pub percent_b: f64,   // %B
}

impl BollingerBands {
    pub fn new(period: usize, multiplier: f64) -> Self {
        Self {
            period,
            multiplier,
            buffer: VecDeque::with_capacity(period),
        }
    }

    /// 기본 설정 (20, 2.0)
    pub fn default() -> Self {
        Self::new(20, 2.0)
    }

    pub fn next(&mut self, close: f64) -> Option<BollingerResult> {
        self.buffer.push_back(close);
        if self.buffer.len() > self.period {
            self.buffer.pop_front();
        }

        if self.buffer.len() < self.period {
            return None;
        }

        let mean = self.buffer.iter().sum::<f64>() / self.period as f64;

        // 표준편차 계산
        let variance = self.buffer.iter()
            .map(|x| (x - mean).powi(2))
            .sum::<f64>() / self.period as f64;
        let std_dev = variance.sqrt();

        let upper = mean + self.multiplier * std_dev;
        let lower = mean - self.multiplier * std_dev;
        let band_width = upper - lower;

        let bandwidth = if mean != 0.0 {
            band_width / mean * 100.0
        } else {
            0.0
        };

        let percent_b = if band_width != 0.0 {
            (close - lower) / band_width
        } else {
            0.5
        };

        Some(BollingerResult {
            upper,
            middle: mean,
            lower,
            bandwidth,
            percent_b,
        })
    }
}
```

- **필요 최소 데이터**: n개 (기본 20)
- **시간복잡도**: O(n) per update (표준편차 계산)
- **공간복잡도**: O(n) (버퍼 크기 = period)
- **최적화**: Welford의 온라인 알고리즘으로 O(1) 가능하나, n=20으로 충분히 빠름

## 관련 문서

- [이동평균선](moving-averages.md) — SMA가 중심선
- [RSI](rsi.md) — 볼린저 + RSI 조합 전략
- [지표 조합 전략](strategies.md) — 평균회귀 전략
