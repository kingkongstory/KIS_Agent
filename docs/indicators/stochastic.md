> [docs/README.md](../README.md) > [indicators/](./overview.md) > stochastic.md

# 스토캐스틱 (Stochastic Oscillator)

## 의미와 용도

스토캐스틱은 George Lane이 1950년대에 개발한 모멘텀 지표로, 현재 종가가 일정 기간의 가격 범위에서 **어디에 위치하는지**를 0~100 범위로 표현한다.

**핵심 개념**: "상승 추세에서는 종가가 고가 근처에서 형성되고, 하락 추세에서는 저가 근처에서 형성된다"

## 구성 요소

| 구성 요소 | 역할 | 설명 |
|-----------|------|------|
| **%K** | 주 지표 | 현재 가격의 위치 백분율 |
| **%D** | 시그널 | %K의 이동평균 (매매 타이밍) |

## Fast vs Slow 스토캐스틱

| 종류 | %K | %D | 특징 |
|------|-----|-----|------|
| **Fast** | 원래 %K | Fast %K의 SMA(3) | 노이즈 많음, 빈번한 시그널 |
| **Slow** | Fast %D | Slow %K의 SMA(3) | 부드러움, **실전에서 주로 사용** |

> 실무에서는 **Slow 스토캐스틱**을 기본으로 사용한다.

## 계산식

### Fast 스토캐스틱

**%K (Fast)**
```
%K = (현재 종가 - n일 최저가) / (n일 최고가 - n일 최저가) × 100
```

**%D (Fast)**
```
%D = SMA(%K, m)
```

기본값: n = 14, m = 3

### Slow 스토캐스틱

```
Slow %K = Fast %D = SMA(Fast %K, m)
Slow %D = SMA(Slow %K, m)
```

### 계산 예시 (14일, 3일)

| 일자 | 종가 | 14일 최고 | 14일 최저 | Fast %K | Fast %D (Slow %K) |
|------|------|----------|----------|---------|-------------------|
| 14일 | 105 | 110 | 95 | (105-95)/(110-95)×100 = **66.7** | - |
| 15일 | 108 | 110 | 95 | (108-95)/(110-95)×100 = **86.7** | - |
| 16일 | 103 | 110 | 95 | (103-95)/(110-95)×100 = **53.3** | **(66.7+86.7+53.3)/3 = 68.9** |
| 17일 | 106 | 110 | 96 | (106-96)/(110-96)×100 = **71.4** | (86.7+53.3+71.4)/3 = 70.5 |

## 해석 기준

| %K 범위 | 의미 | 행동 |
|---------|------|------|
| **80 이상** | 과매수 영역 | 매도 고려 |
| **50** | 중립 | 추세 판단 |
| **20 이하** | 과매도 영역 | 매수 고려 |

### RSI와의 비교

| 항목 | RSI | 스토캐스틱 |
|------|-----|-----------|
| 측정 대상 | 가격 **변동**의 강도 | 가격 **위치** (범위 대비) |
| 입력 | 종가만 | 고가, 저가, 종가 |
| 반응 속도 | 보통 | 빠름 |
| 과매수 기준 | 70 | 80 |
| 과매도 기준 | 30 | 20 |
| 노이즈 | 적음 | 많음 (Slow로 보완) |

두 지표를 함께 사용하면 상호 보완이 된다.

## 매매 시그널

### 1. %K / %D 교차

| 조건 | 시그널 |
|------|--------|
| %K가 %D를 **상향 돌파** (과매도 영역에서) | **매수** |
| %K가 %D를 **하향 돌파** (과매수 영역에서) | **매도** |

> **핵심**: 교차가 과매수/과매도 영역에서 발생해야 의미가 있다. 중간 영역(30~70) 교차는 신뢰도가 낮다.

### 2. 과매수/과매도 이탈

| 조건 | 시그널 |
|------|--------|
| %K가 20 아래에서 20을 **상향 돌파** | 매수 (과매도 탈출) |
| %K가 80 위에서 80을 **하향 돌파** | 매도 (과매수 탈출) |

### 3. 다이버전스

RSI와 동일한 원리:
- **강세 다이버전스**: 가격 저점 갱신 + %K 저점 미갱신 → 반등 가능
- **약세 다이버전스**: 가격 고점 갱신 + %K 고점 미갱신 → 하락 가능

## Rust 구현 가이드

```rust
pub struct Stochastic {
    k_period: usize,   // %K 기간 (기본 14)
    d_period: usize,   // %D 기간 (기본 3)
    slow: bool,        // Slow 스토캐스틱 여부
    high_buffer: VecDeque<f64>,
    low_buffer: VecDeque<f64>,
    fast_k_sma: Sma,   // Fast %D = Slow %K 계산용
    slow_d_sma: Option<Sma>, // Slow %D 계산용
}

#[derive(Debug, Clone)]
pub struct StochasticResult {
    pub k: f64,  // %K
    pub d: f64,  // %D
}

impl Stochastic {
    pub fn new(k_period: usize, d_period: usize, slow: bool) -> Self {
        Self {
            k_period,
            d_period,
            slow,
            high_buffer: VecDeque::with_capacity(k_period),
            low_buffer: VecDeque::with_capacity(k_period),
            fast_k_sma: Sma::new(d_period),
            slow_d_sma: if slow { Some(Sma::new(d_period)) } else { None },
        }
    }

    /// 기본 Slow 스토캐스틱 (14, 3)
    pub fn default_slow() -> Self {
        Self::new(14, 3, true)
    }

    pub fn next(&mut self, high: f64, low: f64, close: f64) -> Option<StochasticResult> {
        self.high_buffer.push_back(high);
        self.low_buffer.push_back(low);

        if self.high_buffer.len() > self.k_period {
            self.high_buffer.pop_front();
            self.low_buffer.pop_front();
        }

        if self.high_buffer.len() < self.k_period {
            return None;
        }

        // Fast %K 계산
        let highest = self.high_buffer.iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        let lowest = self.low_buffer.iter()
            .cloned()
            .fold(f64::INFINITY, f64::min);

        let range = highest - lowest;
        let fast_k = if range > 0.0 {
            (close - lowest) / range * 100.0
        } else {
            50.0 // 범위가 0이면 중간값
        };

        if self.slow {
            // Slow 스토캐스틱
            // Slow %K = Fast %D = SMA(Fast %K, d_period)
            let slow_k = self.fast_k_sma.next(fast_k)?;
            let slow_d = self.slow_d_sma.as_mut()
                .and_then(|sma| sma.next(slow_k))?;
            Some(StochasticResult { k: slow_k, d: slow_d })
        } else {
            // Fast 스토캐스틱
            let fast_d = self.fast_k_sma.next(fast_k)?;
            Some(StochasticResult { k: fast_k, d: fast_d })
        }
    }
}
```

- **필요 최소 데이터**:
  - Fast: k_period + d_period - 1 = 14 + 3 - 1 = **16개**
  - Slow: k_period + 2 × d_period - 2 = 14 + 4 = **18개**
- **시간복잡도**: O(n) per update (최고가/최저가 탐색)
- **공간복잡도**: O(n) (고가/저가 버퍼)
- **입력**: 고가, 저가, 종가 (다른 지표와 달리 3개 입력)

## 관련 문서

- [RSI](rsi.md) — 유사한 과매수/과매도 오실레이터
- [이동평균선](moving-averages.md) — %D 계산에 SMA 사용
- [지표 조합 전략](strategies.md) — RSI + 스토캐스틱 이중 확인
