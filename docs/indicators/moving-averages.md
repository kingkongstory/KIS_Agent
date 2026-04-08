> [docs/README.md](../README.md) > [indicators/](./overview.md) > moving-averages.md

# 이동평균선 (Moving Averages)

## 의미와 용도

이동평균선은 일정 기간의 종가 평균을 이어 만든 곡선이다. 가격 데이터의 노이즈를 제거하고 **추세 방향**을 직관적으로 파악할 수 있게 해준다.

**주요 역할**:
- 추세 방향 확인 (상승/하락/횡보)
- 동적 지지선/저항선
- 골든크로스/데드크로스 시그널 생성
- 다른 지표(MACD, 볼린저밴드)의 기반 구성 요소

## SMA (Simple Moving Average, 단순이동평균)

### 정의

n일 동안의 종가를 단순 산술 평균한 값:

```
SMA(n) = (C₁ + C₂ + ... + Cₙ) / n
```

여기서 C는 종가(Close)

### 계산 예시 (5일 SMA)

| 일자 | 종가 | SMA(5) |
|------|------|--------|
| 1일 | 100 | - |
| 2일 | 102 | - |
| 3일 | 104 | - |
| 4일 | 103 | - |
| 5일 | 105 | **(100+102+104+103+105)/5 = 102.8** |
| 6일 | 107 | (102+104+103+105+107)/5 = 104.2 |
| 7일 | 106 | (104+103+105+107+106)/5 = 105.0 |

### 장단점

- **장점**: 계산이 단순하고 이해하기 쉬움
- **단점**: 모든 데이터에 동일 가중치를 부여하여 최신 변화에 느리게 반응 (후행성)

## EMA (Exponential Moving Average, 지수이동평균)

### 정의

최근 데이터에 더 높은 가중치를 부여하는 이동평균:

```
EMA(t) = Close(t) × k + EMA(t-1) × (1 - k)

k = 2 / (n + 1)    (평활 계수)
```

### 계산 예시 (5일 EMA, k = 2/6 ≈ 0.333)

| 일자 | 종가 | EMA(5) | 계산 |
|------|------|--------|------|
| 1~5일 | - | 102.8 | 초기값 = SMA(5) |
| 6일 | 107 | 104.2 | 107 × 0.333 + 102.8 × 0.667 |
| 7일 | 106 | 104.8 | 106 × 0.333 + 104.2 × 0.667 |

> **초기값**: 첫 n개 데이터의 SMA를 EMA 초기값으로 사용

### SMA vs EMA 비교

| 항목 | SMA | EMA |
|------|-----|-----|
| 최근 가격 반응 | 느림 | 빠름 |
| 노이즈 필터링 | 강함 | 약함 |
| 계산 복잡도 | O(n) 슬라이딩 윈도우 | O(1) 상태 유지 |
| 용도 | 장기 추세, 볼린저밴드 | MACD, 단기 매매 |

## WMA (Weighted Moving Average, 가중이동평균)

### 정의

최근 데이터에 선형적으로 더 큰 가중치를 부여:

```
WMA(n) = (n×Cₙ + (n-1)×Cₙ₋₁ + ... + 1×C₁) / (n + (n-1) + ... + 1)
       = Σ(i × Cᵢ) / (n × (n+1) / 2)
```

EMA보다 덜 사용되지만, EMA보다 직관적인 가중 방식이다.

## 주요 기간 설정

| 기간 | 의미 | 용도 |
|------|------|------|
| **5일** | 1주 거래일 | 초단기 추세, 데이트레이딩 |
| **10일** | 2주 | 단기 추세 |
| **20일** | 1개월 | 단기~중기 추세, 볼린저밴드 중심선 |
| **60일** | 분기 | 중기 추세 |
| **120일** | 반기 | 중장기 추세 (한국시장 주요선) |
| **200일** | 약 10개월 | 장기 추세 (글로벌 기준선) |

> 한국 주식시장 거래일은 연간 약 250일.

## 매매 시그널

### 골든크로스 (Golden Cross)

**단기 MA가 장기 MA를 아래에서 위로 돌파** → **매수 시그널**

```
가격 ─────────────╱──── ← 단기 MA (위로)
                 ╳
장기 MA ────────╱────── ← 장기 MA (아래)
            골든크로스
```

- 대표적: 5일선이 20일선 상향 돌파, 20일선이 60일선 상향 돌파
- 강도: 장기선끼리의 크로스일수록 신뢰도 높음 (50일 & 200일 > 5일 & 20일)

### 데드크로스 (Dead Cross)

**단기 MA가 장기 MA를 위에서 아래로 돌파** → **매도 시그널**

골든크로스의 반대 방향.

### 가격과 MA의 관계

| 상황 | 의미 |
|------|------|
| 가격 > MA | 현재 추세보다 강세 (MA가 지지선) |
| 가격 < MA | 현재 추세보다 약세 (MA가 저항선) |
| 가격이 MA에 접근 후 반등 | 지지 확인 |
| 가격이 MA를 하향 이탈 | 추세 전환 가능 |

### 다중 이동평균 배열

| 배열 | 조건 | 의미 |
|------|------|------|
| **정배열** | MA5 > MA20 > MA60 > MA120 | 강한 상승 추세 |
| **역배열** | MA5 < MA20 < MA60 < MA120 | 강한 하락 추세 |
| **수렴** | MA들이 한 점으로 모임 | 큰 움직임 예고 |

## Rust 구현 가이드

### SMA 구현

```rust
pub struct Sma {
    period: usize,
    buffer: VecDeque<f64>,
    sum: f64,
}

impl Sma {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            buffer: VecDeque::with_capacity(period),
            sum: 0.0,
        }
    }

    /// 새 값 추가, 충분한 데이터가 있으면 SMA 반환
    pub fn next(&mut self, value: f64) -> Option<f64> {
        self.buffer.push_back(value);
        self.sum += value;

        if self.buffer.len() > self.period {
            self.sum -= self.buffer.pop_front().unwrap();
        }

        if self.buffer.len() == self.period {
            Some(self.sum / self.period as f64)
        } else {
            None
        }
    }
}
```

- **시간복잡도**: O(1) per update (슬라이딩 윈도우)
- **공간복잡도**: O(n) (버퍼 크기 = period)

### EMA 구현

```rust
pub struct Ema {
    period: usize,
    k: f64,          // 평활 계수
    current: Option<f64>,
    count: usize,
    sma: Sma,        // 초기값 계산용
}

impl Ema {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            k: 2.0 / (period as f64 + 1.0),
            current: None,
            count: 0,
            sma: Sma::new(period),
        }
    }

    pub fn next(&mut self, value: f64) -> Option<f64> {
        self.count += 1;

        match self.current {
            Some(prev) => {
                let ema = value * self.k + prev * (1.0 - self.k);
                self.current = Some(ema);
                Some(ema)
            }
            None => {
                // 초기값: SMA로 계산
                if let Some(sma) = self.sma.next(value) {
                    self.current = Some(sma);
                    Some(sma)
                } else {
                    None
                }
            }
        }
    }
}
```

- **시간복잡도**: O(1) per update
- **공간복잡도**: O(1) (초기화 후 이전 값만 보관)
- **필요 최소 데이터**: n개 (period)

## 관련 문서

- [MACD](macd.md) — EMA(12)와 EMA(26)을 기반으로 계산
- [볼린저밴드](bollinger-bands.md) — SMA(20)을 중심선으로 사용
- [기술적 분석 개요](overview.md) — Indicator trait 정의
- [지표 조합 전략](strategies.md) — MA 크로스 전략
