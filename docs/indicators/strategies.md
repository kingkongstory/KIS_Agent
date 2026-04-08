> [docs/README.md](../README.md) > [indicators/](./overview.md) > strategies.md

# 지표 조합 전략

## KIS 프리셋 전략 (10개)

KIS Strategy Builder에 내장된 10개 프리셋 전략이다. 우리 프로젝트에서 전략 구현의 참조가 된다.

> **출처**: https://github.com/koreainvestment/open-trading-api/tree/main/strategy_builder/strategy_core/preset

| # | 전략명 | 카테고리 | 핵심 지표 | 매수 조건 | 매도 조건 |
|---|--------|---------|----------|----------|----------|
| 1 | **골든크로스** | 추세추종 | SMA(5), SMA(20) | 단기MA가 장기MA 상향 돌파 | 단기MA가 장기MA 하향 돌파 |
| 2 | **모멘텀** | 추세추종 | ROC(60) | ROC > 30% | ROC < -20% |
| 3 | **추세 필터** | 추세추종 | SMA(60), 변화율 | 종가 > SMA & 변화율 > 0 | 종가 < SMA & 변화율 < 0 |
| 4 | **52주 신고가** | 돌파 | 252일 최고가 | 종가 > 52주 최고가 | 종가 < 52주 최고가 |
| 5 | **연속 상승/하락** | 추세추종 | 연속일수 | 5일 연속 상승 | 5일 연속 하락 |
| 6 | **이격도** | 역추세 | 이격도(20) | 이격도 < 90 (과매도) | 이격도 > 110 (과매수) |
| 7 | **평균회귀** | 역추세 | 이격도(5) | 이격도 ≤ 97 (-3% 이탈) | 이격도 ≥ 103 (+3% 이탈) |
| 8 | **돌파 실패** | 역추세 | 20일 최고가 | 종가 < 전고점 | 종가 > 전고점 |
| 9 | **강한 종가** | 모멘텀 | IBS (강한종가비율) | IBS ≥ 0.8 | IBS < 0.5 |
| 10 | **변동성 확장** | 돌파 | 변동성(10), 변화율 | 변동성 ≤ 0.02 & 변화율 ≥ 3% | 변화율 ≤ -3% |

### 프리셋별 리스크 관리 설정

| 전략 | 손절 | 익절 | 트레일링 |
|------|------|------|---------|
| 골든크로스 | 5% | - | - |
| 모멘텀 | 5% | - | - |
| 추세 필터 | 5% | - | - |
| 52주 신고가 | 5% | 15% | - |
| 연속 상승/하락 | 5% | - | - |
| 이격도 | 5% | 10% | - |
| 평균회귀 | 3% | 3% | - |
| 돌파 실패 | 3% | - | - |
| 강한 종가 | 3% | 5% | - |
| 변동성 확장 | 3% | - | - |

### .kis.yaml 전략 형식

프리셋 전략은 `.kis.yaml` 형식으로 내보내기/수정이 가능하다. 이 형식을 우리 Rust 프로젝트의 전략 설정에도 채택할 수 있다.

```yaml
name: "골든크로스"
category: "trend"
indicators:
  - name: SMA
    alias: sma_fast
    params: { period: 5, source: close }
  - name: SMA
    alias: sma_slow
    params: { period: 20, source: close }
entry:
  logic: AND
  conditions:
    - { left: sma_fast, op: cross_above, right: sma_slow }
exit:
  logic: OR
  conditions:
    - { left: sma_fast, op: cross_below, right: sma_slow }
risk:
  stop_loss: { enabled: true, pct: 5.0 }
  take_profit: { enabled: false, pct: 10.0 }
  trailing_stop: { enabled: false, pct: 3.0 }
```

> 상세: [KIS GitHub 리소스](../api/github-resources.md)

---

## 전략 설계 원칙

### 단일 지표의 한계

어떤 지표든 단독으로 사용하면 **거짓 신호(false signal)**가 빈번하다:

- 이동평균 크로스: 횡보장에서 반복적 거짓 크로스 (whipsaw)
- RSI 과매수: 강한 상승장에서는 70 이상을 유지하면서도 계속 상승
- 볼린저 하단 접촉: 하락 추세에서는 하단을 따라 계속 하락 (밴드 워킹)

### 다중 지표 확인 (Confirmation) 원칙

서로 다른 카테고리의 지표를 조합하여 신뢰도를 높인다:

```
추세 지표 (방향)  +  모멘텀 지표 (강도)  +  거래량 지표 (신뢰도)
    MA, MACD           RSI, 스토캐스틱         OBV, VWAP
```

**원칙**: 같은 카테고리 지표끼리 조합하면 중복 정보이므로 의미가 적다. (예: RSI + 스토캐스틱은 둘 다 모멘텀이라 비슷한 신호)

## 전략 1: 추세추종 (MA + MACD + OBV)

### 개념

강한 추세를 확인하고 그 방향으로 매매한다.

### 매수 조건 (모두 충족 시)

| # | 조건 | 지표 | 역할 |
|---|------|------|------|
| 1 | 5일선 > 20일선 (골든크로스 발생 또는 유지) | MA | 추세 방향 |
| 2 | MACD > 시그널 (또는 상향 돌파) | MACD | 모멘텀 확인 |
| 3 | OBV 상승 추세 | OBV | 거래량 뒷받침 |

### 매도 조건 (하나라도 충족 시)

| # | 조건 | 의미 |
|---|------|------|
| 1 | 5일선 < 20일선 (데드크로스) | 추세 전환 |
| 2 | MACD < 시그널 (하향 돌파) | 모멘텀 약화 |
| 3 | 손절: 매수가 대비 -3% | 리스크 관리 |

### 적합한 시장

- 뚜렷한 상승/하락 추세가 있는 장
- 횡보장에서는 거짓 신호가 많음

## 전략 2: 평균회귀 (볼린저밴드 + RSI)

### 개념

가격이 평균에서 과도하게 벗어나면 다시 돌아온다는 전제.

### 매수 조건 (모두 충족 시)

| # | 조건 | 지표 | 역할 |
|---|------|------|------|
| 1 | 가격이 볼린저 하단밴드 접촉 또는 이탈 | BB | 과도한 하락 |
| 2 | RSI < 30 (과매도) | RSI | 모멘텀 확인 |
| 3 | (선택) 거래량 감소 추세 | Volume | 매도세 소진 |

### 매도 조건

| # | 조건 | 의미 |
|---|------|------|
| 1 | 가격이 볼린저 중심선(SMA20) 도달 | 평균 회귀 완료 |
| 2 | RSI > 50 | 모멘텀 중립 회복 |
| 3 | (공격적) 볼린저 상단 접촉 시 | 과매수 영역 |

### 강한 매수 시그널

```
볼린저 하단 이탈 + RSI < 25 + 양봉 출현 = 강한 반등 기대
```

### 적합한 시장

- 횡보장, 박스권 장세
- 추세장에서는 위험 (밴드 워킹으로 손실 가능)

## 전략 3: 모멘텀 (RSI + MACD)

### 개념

모멘텀이 전환되는 시점을 포착한다.

### 매수 조건

| # | 조건 | 역할 |
|---|------|------|
| 1 | RSI가 30 아래에서 30을 상향 돌파 | 과매도 탈출 |
| 2 | MACD 히스토그램이 음수에서 양수로 전환 | 모멘텀 전환 |

### 매도 조건

| # | 조건 | 역할 |
|---|------|------|
| 1 | RSI가 70 위에서 70을 하향 돌파 | 과매수 탈출 |
| 2 | MACD 히스토그램이 양수에서 음수로 전환 | 모멘텀 약화 |

## 전략 4: 돌파 확인 (가격 + 거래량)

### 개념

가격이 주요 지지/저항선을 돌파할 때, 거래량으로 돌파의 진위를 확인한다.

### 매수 조건

| # | 조건 | 의미 |
|---|------|------|
| 1 | 가격이 20일 고점 돌파 | 저항선 돌파 |
| 2 | 거래량이 20일 평균 거래량의 1.5배 이상 | 강한 매수세 |
| 3 | 볼린저 상단 돌파 + 밴드폭 확대 | 변동성 증가 확인 |

### 거짓 돌파 필터

- 돌파 후 1~2일 유지 여부 확인
- 종가 기준 돌파만 인정 (장중 돌파는 무시)
- OBV도 함께 고점 돌파 시 신뢰도 높음

## 차트 패턴

### 이중 바닥 (Double Bottom)

```
가격: ╲      ╱╲      ╱
       ╲    ╱  ╲    ╱
        ╲──╱    ╲──╱
         W        W
       1차 저점  2차 저점 (비슷한 가격)
```

- 1차 저점과 2차 저점의 가격이 비슷
- 2차 저점에서 RSI가 1차보다 높으면 (다이버전스) 신뢰도 상승
- **넥라인** (두 저점 사이의 고점) 돌파 시 매수

### 이중 천정 (Double Top)

이중 바닥의 반대. 넥라인 하향 이탈 시 매도.

### 헤드앤숄더 (Head and Shoulders)

```
가격:     ╱╲
        ╱╲╱ ╲╱╲
       ╱  왼   오 ╲
      ╱   어   른  ╲
     ╱    깨   쪽   ╲
         ── 넥라인 ──
```

넥라인 하향 이탈 시 하락 추세 전환 시그널.

### 패턴 인식 로직 (개념)

```rust
/// 이중 바닥 감지
fn detect_double_bottom(candles: &[Candle], threshold: f64) -> Option<Signal> {
    // 1. 최근 N개 캔들에서 두 개의 저점 찾기
    // 2. 두 저점의 가격 차이가 threshold 이내인지 확인
    // 3. 두 저점 사이에 의미 있는 반등(넥라인)이 있는지 확인
    // 4. 현재가가 넥라인을 돌파했는지 확인
    // 5. (선택) RSI 다이버전스 확인
    todo!()
}
```

> 차트 패턴 인식은 지표 계산보다 복잡하며, 정확한 구현은 개발 단계에서 세부 설계한다.

## 시그널 엔진 설계

### Signal 타입

```rust
#[derive(Debug, Clone)]
pub struct TradeSignal {
    pub stock_code: StockCode,
    pub signal: Signal,          // StrongBuy, Buy, Hold, Sell, StrongSell
    pub confidence: f64,         // 신뢰도 0.0 ~ 1.0
    pub source: Vec<String>,     // 시그널 근거 (어떤 지표가 발생시켰는지)
    pub timestamp: DateTime<Utc>,
}
```

### 다중 전략 병합

```rust
/// 복수 전략의 시그널을 종합
fn merge_signals(signals: &[TradeSignal]) -> TradeSignal {
    // 가중 평균 방식
    let mut buy_score: f64 = 0.0;
    let mut sell_score: f64 = 0.0;
    let mut total_weight: f64 = 0.0;

    for signal in signals {
        let weight = signal.confidence;
        total_weight += weight;
        match signal.signal {
            Signal::StrongBuy => buy_score += 2.0 * weight,
            Signal::Buy => buy_score += 1.0 * weight,
            Signal::Hold => {},
            Signal::Sell => sell_score += 1.0 * weight,
            Signal::StrongSell => sell_score += 2.0 * weight,
        }
    }

    let net_score = (buy_score - sell_score) / total_weight;
    let final_signal = match net_score {
        s if s > 1.5 => Signal::StrongBuy,
        s if s > 0.5 => Signal::Buy,
        s if s < -1.5 => Signal::StrongSell,
        s if s < -0.5 => Signal::Sell,
        _ => Signal::Hold,
    };

    TradeSignal {
        signal: final_signal,
        confidence: net_score.abs() / 2.0, // 정규화
        source: signals.iter().flat_map(|s| s.source.clone()).collect(),
        // ...
    }
}
```

### Strategy trait

```rust
pub trait Strategy {
    /// 전략 이름
    fn name(&self) -> &str;

    /// 캔들 데이터로 시그널 생성
    fn evaluate(&self, candles: &[Candle]) -> Result<TradeSignal, StrategyError>;

    /// 전략 파라미터 설명
    fn parameters(&self) -> HashMap<String, String>;
}
```

### 전략 파이프라인

```
OHLCV 수집 → 지표 계산 (병렬) → 전략별 시그널 생성 → 시그널 병합 → 매매 결정
                                                          ↓
                                                 리스크 관리 필터
                                                 (최대 포지션, 손절 등)
                                                          ↓
                                                     주문 실행
```

## 리스크 관리

어떤 전략이든 리스크 관리 없이는 위험하다:

| 규칙 | 설명 |
|------|------|
| **손절 (Stop Loss)** | 매수가 대비 -3%~-5% 하락 시 자동 매도 |
| **익절 (Take Profit)** | 목표 수익률 도달 시 일부/전량 매도 |
| **포지션 크기** | 총 자산의 10~20% 이하로 단일 종목 투자 |
| **최대 포지션** | 동시에 보유할 수 있는 종목 수 제한 |
| **일일 거래 횟수** | 과도한 매매 방지 (수수료 + 슬리피지) |

```rust
struct RiskManager {
    max_position_ratio: f64,   // 단일 종목 최대 비율 (예: 0.2 = 20%)
    stop_loss_pct: f64,        // 손절 비율 (예: -0.03 = -3%)
    take_profit_pct: f64,      // 익절 비율 (예: 0.10 = +10%)
    max_positions: usize,      // 최대 보유 종목 수
}

impl RiskManager {
    /// 시그널이 리스크 기준을 통과하는지 검증
    fn filter(&self, signal: &TradeSignal, portfolio: &Portfolio) -> bool {
        match signal.signal {
            Signal::Buy | Signal::StrongBuy => {
                // 이미 최대 포지션이면 매수 차단
                portfolio.positions.len() < self.max_positions
            }
            _ => true,
        }
    }
}
```

## 백테스트

전략을 실전에 적용하기 전, 과거 데이터로 반드시 검증한다:

```rust
struct BacktestResult {
    total_trades: usize,       // 총 거래 횟수
    winning_trades: usize,     // 수익 거래
    losing_trades: usize,      // 손실 거래
    win_rate: f64,             // 승률 (%)
    total_return: f64,         // 총 수익률 (%)
    max_drawdown: f64,         // 최대 낙폭 (%)
    sharpe_ratio: f64,         // 샤프 비율
}
```

> 백테스트 프레임워크의 세부 설계는 개발 단계에서 구체화한다.

## 관련 문서

- 개별 지표: [MA](moving-averages.md), [RSI](rsi.md), [MACD](macd.md), [볼린저밴드](bollinger-bands.md), [스토캐스틱](stochastic.md), [거래량](volume-indicators.md)
- [기술적 분석 개요](overview.md) — Indicator trait, Signal enum
- [국내주식 주문](../api/domestic-stock-trading.md) — 시그널 → 주문 실행
