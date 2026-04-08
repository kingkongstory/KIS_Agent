> [docs/README.md](../README.md) > [api/](./overview.md) > data-types.md

# 데이터 타입 매핑 (KIS → Rust)

KIS API는 응답에서 숫자를 문자열로 반환하는 등 독특한 데이터 관행이 있다. Rust에서 serde 역직렬화 시 이를 올바르게 처리해야 한다.

## KIS API 데이터 특성

### 1. 숫자가 문자열로 반환됨

KIS API 응답의 대부분의 숫자 필드가 문자열로 감싸져 온다:

```json
{
  "stck_prpr": "72500",      // 현재가 — 숫자가 아닌 문자열
  "prdy_vrss": "-500",       // 전일대비 — 음수도 문자열
  "acml_vol": "12345678",    // 거래량 — 문자열
  "prdy_ctrt": "-0.68"       // 등락률 — 소수점도 문자열
}
```

### 2. 날짜/시간 형식

| 형식 | 예시 | 용도 |
|------|------|------|
| `YYYYMMDD` | `"20260408"` | 일자 (시세, 주문) |
| `HHMMSS` | `"153000"` | 시각 (체결시각 등) |
| `YYYY-MM-DD HH:MM:SS` | `"2026-04-09 12:00:00"` | 토큰 만료 시각 |

### 3. 빈 문자열 vs null

- 데이터 없음: 빈 문자열 `""` 또는 `"0"`으로 반환 (null이 아님)
- 일부 필드만 `null` 가능
- `Option<T>` 처리 시 빈 문자열도 `None`으로 변환해야 하는 경우 있음

### 4. 부호 문자

전일대비 등에서 부호를 별도 필드로 제공하기도 한다:

```json
{
  "prdy_vrss_sign": "5",  // 1:상한 2:상승 3:보합 4:하한 5:하락
  "prdy_vrss": "500"      // 절대값 (부호 없음)
}
```

| 부호 코드 | 의미 |
|-----------|------|
| `1` | 상한 |
| `2` | 상승 |
| `3` | 보합 |
| `4` | 하한 |
| `5` | 하락 |

## serde 역직렬화 전략

### 문자열 → 숫자 변환

커스텀 역직렬화 모듈을 만들어 재사용한다:

```rust
/// 문자열로 된 숫자를 i64로 변환
mod string_to_i64 {
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<i64>().map_err(serde::de::Error::custom)
    }
}

/// 문자열로 된 숫자를 f64로 변환
mod string_to_f64 {
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<f64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<f64>().map_err(serde::de::Error::custom)
    }
}

/// 빈 문자열 또는 숫자 문자열 → Option<i64>
mod optional_string_to_i64 {
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            Ok(None)
        } else {
            s.parse::<i64>()
                .map(Some)
                .map_err(serde::de::Error::custom)
        }
    }
}
```

### 사용 예시

```rust
#[derive(Deserialize)]
struct StockPrice {
    /// 현재가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_prpr: i64,

    /// 전일대비율 (%)
    #[serde(deserialize_with = "string_to_f64::deserialize")]
    prdy_ctrt: f64,

    /// 누적 거래량
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    acml_vol: i64,
}
```

### 날짜 파싱

```rust
use chrono::NaiveDate;

/// KIS "YYYYMMDD" 형식 → NaiveDate
mod kis_date {
    use chrono::NaiveDate;
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveDate, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveDate::parse_from_str(&s, "%Y%m%d").map_err(serde::de::Error::custom)
    }
}

/// KIS "HHMMSS" 형식 → NaiveTime
mod kis_time {
    use chrono::NaiveTime;
    use serde::{self, Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<NaiveTime, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        NaiveTime::parse_from_str(&s, "%H%M%S").map_err(serde::de::Error::custom)
    }
}
```

## 공통 타입 정의

프로젝트 전체에서 사용할 도메인 타입:

```rust
/// 종목코드 (6자리 문자열, 예: "005930")
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StockCode(String);

impl StockCode {
    pub fn new(code: &str) -> Result<Self, KisError> {
        if code.len() == 6 && code.chars().all(|c| c.is_ascii_digit()) {
            Ok(Self(code.to_string()))
        } else {
            Err(KisError::InvalidStockCode(code.to_string()))
        }
    }
}

/// 시장 구분 코드
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MarketCode {
    Stock,  // J — 주식/ETF/ETN
    Elw,    // W — ELW
}

impl MarketCode {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Stock => "J",
            Self::Elw => "W",
        }
    }
}

/// OHLCV 캔들 데이터 (기술적 지표 계산의 기본 단위)
#[derive(Debug, Clone)]
pub struct Candle {
    pub date: NaiveDate,
    pub open: i64,    // 시가
    pub high: i64,    // 고가
    pub low: i64,     // 저가
    pub close: i64,   // 종가
    pub volume: u64,  // 거래량
}
```

## 부호 코드 처리

```rust
/// 전일대비 부호
#[derive(Debug, Clone)]
pub enum PriceChangeSign {
    UpperLimit,  // 1: 상한
    Rise,        // 2: 상승
    Flat,        // 3: 보합
    LowerLimit,  // 4: 하한
    Fall,        // 5: 하락
}

impl PriceChangeSign {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "1" => Some(Self::UpperLimit),
            "2" => Some(Self::Rise),
            "3" => Some(Self::Flat),
            "4" => Some(Self::LowerLimit),
            "5" => Some(Self::Fall),
            _ => None,
        }
    }

    /// 실제 변동값에 부호를 적용
    pub fn apply_sign(&self, value: i64) -> i64 {
        match self {
            Self::Fall | Self::LowerLimit => -value,
            _ => value,
        }
    }
}
```

## 관련 문서

- [API 공통사항](overview.md) — 응답 공통 형식
- [국내주식 시세](domestic-stock-quotations.md) — 시세 응답 필드 상세
- [에러 처리](error-handling.md) — 에러 응답 파싱
