> [docs/README.md](../README.md) > [api/](./overview.md) > domestic-stock-quotations.md

# 국내주식 시세 조회

시세 데이터는 기술적 지표 계산의 원재료이다. 이 문서에서 다루는 OHLCV 데이터가 [기술적 분석](../indicators/overview.md) 전체의 입력이 된다.

## 현재가 조회

### 엔드포인트

```
GET /uapi/domestic-stock/v1/quotations/inquire-price
```

**tr_id**: `FHKST01010100` (실전/모의 동일)

### 요청 파라미터 (Query String)

| 파라미터 | 타입 | 필수 | 설명 | 예시 |
|---------|------|------|------|------|
| `FID_COND_MRKT_DIV_CODE` | String | O | 시장 구분 | `J` (주식), `W` (ELW) |
| `FID_INPUT_ISCD` | String | O | 종목코드 6자리 | `005930` (삼성전자) |

### 응답 주요 필드 (output)

| 필드명 | 한국어명 | 설명 |
|--------|---------|------|
| `stck_prpr` | 주식현재가 | 현재가 |
| `prdy_vrss` | 전일대비 | 전일 종가 대비 변동 (절대값) |
| `prdy_vrss_sign` | 전일대비부호 | 1:상한 2:상승 3:보합 4:하한 5:하락 |
| `prdy_ctrt` | 전일대비율 | 등락률 (%) |
| `stck_oprc` | 시가 | 당일 시가 |
| `stck_hgpr` | 최고가 | 당일 고가 |
| `stck_lwpr` | 최저가 | 당일 저가 |
| `acml_vol` | 누적거래량 | 당일 누적 거래량 |
| `acml_tr_pbmn` | 누적거래대금 | 당일 누적 거래대금 |
| `stck_mxpr` | 상한가 | 당일 상한가 |
| `stck_llam` | 하한가 | 당일 하한가 |
| `w52_hgpr` | 52주최고가 | |
| `w52_lwpr` | 52주최저가 | |
| `per` | PER | 주가수익비율 |
| `pbr` | PBR | 주가순자산비율 |
| `eps` | EPS | 주당순이익 |
| `bps` | BPS | 주당순자산 |
| `hts_avls` | HTS시가총액 | 시가총액 (억 원) |

### JSON 응답 예시

```json
{
  "rt_cd": "0",
  "msg_cd": "MCA00000",
  "msg1": "정상처리 되었습니다.",
  "output": {
    "stck_prpr": "72500",
    "prdy_vrss": "500",
    "prdy_vrss_sign": "2",
    "prdy_ctrt": "0.69",
    "stck_oprc": "72000",
    "stck_hgpr": "73000",
    "stck_lwpr": "71500",
    "acml_vol": "12345678",
    "acml_tr_pbmn": "896123456789",
    "per": "12.50",
    "pbr": "1.30"
  }
}
```

### Rust 구조체

```rust
#[derive(Debug, Deserialize)]
pub struct InquirePrice {
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub stck_prpr: i64,         // 현재가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub prdy_vrss: i64,         // 전일대비 (절대값)
    pub prdy_vrss_sign: String, // 전일대비부호
    #[serde(deserialize_with = "string_to_f64::deserialize")]
    pub prdy_ctrt: f64,         // 전일대비율 (%)
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub stck_oprc: i64,         // 시가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub stck_hgpr: i64,         // 고가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub stck_lwpr: i64,         // 저가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub acml_vol: i64,          // 누적거래량
}
```

## 호가 조회

### 엔드포인트

```
GET /uapi/domestic-stock/v1/quotations/inquire-asking-price-exp-ccn
```

**tr_id**: `FHKST01010200` (실전/모의 동일)

### 요청 파라미터

현재가 조회와 동일 (`FID_COND_MRKT_DIV_CODE`, `FID_INPUT_ISCD`)

### 응답 주요 필드

매도/매수 10단계 호가 데이터:

| 필드 패턴 | 설명 |
|-----------|------|
| `askp1` ~ `askp10` | 매도호가 1~10단계 (낮은 → 높은) |
| `bidp1` ~ `bidp10` | 매수호가 1~10단계 (높은 → 낮은) |
| `askp_rsqn1` ~ `askp_rsqn10` | 매도호가 잔량 |
| `bidp_rsqn1` ~ `bidp_rsqn10` | 매수호가 잔량 |
| `total_askp_rsqn` | 총 매도호가 잔량 |
| `total_bidp_rsqn` | 총 매수호가 잔량 |

**호가 잔량 분석 활용**:
- 매수잔량 > 매도잔량: 매수세 우위 (상승 가능성)
- 특정 호가에 대량 잔량: 지지선/저항선 형성

## 기간별 시세 (OHLCV)

기술적 지표 계산에 가장 중요한 API이다.

### 엔드포인트

```
GET /uapi/domestic-stock/v1/quotations/inquire-daily-itemchartprice
```

**tr_id**: `FHKST03010100` (실전/모의 동일)

### 요청 파라미터

| 파라미터 | 타입 | 필수 | 설명 | 예시 |
|---------|------|------|------|------|
| `FID_COND_MRKT_DIV_CODE` | String | O | 시장 구분 | `J` |
| `FID_INPUT_ISCD` | String | O | 종목코드 | `005930` |
| `FID_INPUT_DATE_1` | String | O | 조회 시작일 | `20260101` |
| `FID_INPUT_DATE_2` | String | O | 조회 종료일 | `20260408` |
| `FID_PERIOD_DIV_CODE` | String | O | 기간 구분 | `D`(일), `W`(주), `M`(월), `Y`(년) |
| `FID_ORG_ADJ_PRC` | String | O | 수정주가 반영 | `0`(수정주가), `1`(원주가) |

### 응답 주요 필드 (output2 — 배열)

| 필드명 | 한국어명 | 설명 |
|--------|---------|------|
| `stck_bsop_date` | 영업일자 | 날짜 (YYYYMMDD) |
| `stck_oprc` | 시가 | Open |
| `stck_hgpr` | 고가 | High |
| `stck_lwpr` | 저가 | Low |
| `stck_clpr` | 종가 | Close |
| `acml_vol` | 누적거래량 | Volume |
| `acml_tr_pbmn` | 누적거래대금 | |
| `prdy_vrss` | 전일대비 | |
| `prdy_vrss_sign` | 전일대비부호 | |

### JSON 응답 예시

```json
{
  "rt_cd": "0",
  "msg_cd": "MCA00000",
  "msg1": "정상처리 되었습니다.",
  "output1": {
    "prdy_vrss": "500",
    "prdy_vrss_sign": "2",
    "stck_prpr": "72500"
  },
  "output2": [
    {
      "stck_bsop_date": "20260408",
      "stck_oprc": "72000",
      "stck_hgpr": "73000",
      "stck_lwpr": "71500",
      "stck_clpr": "72500",
      "acml_vol": "12345678"
    },
    {
      "stck_bsop_date": "20260407",
      "stck_oprc": "71800",
      "stck_hgpr": "72300",
      "stck_lwpr": "71200",
      "stck_clpr": "72000",
      "acml_vol": "10234567"
    }
  ]
}
```

> **주의**: `output2` 배열은 **최신 날짜가 먼저** (내림차순) 오며, 한 번에 **최대 100건**까지 반환된다.

### 페이징 (연속 조회)

100건 초과 데이터를 가져오려면 연속 조회가 필요하다:

1. 첫 요청 후 응답 헤더에서 연속 조회 키 확인:
   - `tr_cont`: `"F"` = 다음 데이터 있음, `"D"` = 마지막
2. 다음 요청 시 헤더에 연속 조회 키 포함:
   - `tr_cont`: `"N"` (다음 페이지 요청)

### OHLCV 데이터를 Candle로 변환

```rust
impl From<&DailyPriceItem> for Candle {
    fn from(item: &DailyPriceItem) -> Self {
        Candle {
            date: parse_kis_date(&item.stck_bsop_date),
            open: item.stck_oprc,
            high: item.stck_hgpr,
            low: item.stck_lwpr,
            close: item.stck_clpr,
            volume: item.acml_vol as u64,
        }
    }
}
```

### 지표 계산을 위한 데이터 수집 전략

| 지표 | 최소 필요 데이터 수 | 권장 조회 기간 |
|------|-------------------|--------------|
| SMA(20) | 20일 | 30일 |
| EMA(26) | 26일 | 40일 |
| RSI(14) | 15일 | 30일 |
| MACD(12,26,9) | 34일 | 50일 |
| 볼린저밴드(20) | 20일 | 30일 |
| 스토캐스틱(14,3) | 16일 | 30일 |

> 복수 지표를 동시에 계산하려면 가장 긴 기간(MACD 기준 50일)으로 조회하면 된다.

## 실전/모의투자 차이

시세 조회 API는 실전/모의투자 모두 **동일한 tr_id**를 사용한다. 시세 데이터 자체는 실시간 시장 데이터이므로 차이가 없다.

단, 모의투자 환경에서는 호출 빈도 제한이 더 엄격할 수 있다.

## 관련 문서

- [데이터 타입](data-types.md) — 문자열→숫자 변환, Candle 구조체
- [WebSocket](websocket.md) — 실시간 시세 (REST 조회의 실시간 대안)
- [기술적 분석 개요](../indicators/overview.md) — OHLCV 데이터 활용
- [속도 제한](rate-limits.md) — 대량 조회 시 주의사항
