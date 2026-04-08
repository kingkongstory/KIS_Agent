> [docs/README.md](../README.md) > [api/](./overview.md) > account.md

# 계좌 조회

## 잔고 조회

보유 종목 목록, 평가금액, 수익률을 조회한다.

### 엔드포인트

```
GET /uapi/domestic-stock/v1/trading/inquire-balance
```

| 환경 | tr_id |
|------|-------|
| 실전투자 | `TTTC8434R` |
| 모의투자 | `VTTC8434R` |

### 요청 파라미터 (Query String)

| 파라미터 | 타입 | 필수 | 설명 |
|---------|------|------|------|
| `CANO` | String(8) | O | 종합계좌번호 |
| `ACNT_PRDT_CD` | String(2) | O | 계좌상품코드 (`01`) |
| `AFHR_FLPR_YN` | String(1) | O | 시간외단일가여부 (`N`) |
| `OFL_YN` | String(1) | O | 오프라인여부 (`""`) |
| `INQR_DVSN` | String(2) | O | 조회구분 (`01`=대출일별, `02`=종목별) |
| `UNPR_DVSN` | String(2) | O | 단가구분 (`01`) |
| `FUND_STTL_ICLD_YN` | String(1) | O | 펀드결제분포함여부 (`N`) |
| `FNCG_AMT_AUTO_RDPT_YN` | String(1) | O | 융자금액자동상환여부 (`N`) |
| `PRCS_DVSN` | String(2) | O | 처리구분 (`00`=전일매매포함, `01`=전일매매미포함) |
| `CTX_AREA_FK100` | String | O | 연속조회검색조건 (첫 요청 `""`) |
| `CTX_AREA_NK100` | String | O | 연속조회키 (첫 요청 `""`) |

### 응답: output1 (종목별 잔고 — 배열)

| 필드명 | 한국어명 | 설명 |
|--------|---------|------|
| `pdno` | 종목코드 | 6자리 종목코드 |
| `prdt_name` | 종목명 | |
| `hldg_qty` | 보유수량 | |
| `pchs_avg_pric` | 매입평균가격 | |
| `pchs_amt` | 매입금액 | |
| `prpr` | 현재가 | |
| `evlu_amt` | 평가금액 | |
| `evlu_pfls_amt` | 평가손익금액 | |
| `evlu_pfls_rt` | 평가손익률 (%) | |
| `evlu_erng_rt` | 평가수익률 (%) | |

### 응답: output2 (계좌 합계 — 단일 객체)

| 필드명 | 한국어명 | 설명 |
|--------|---------|------|
| `dnca_tot_amt` | 예수금총금액 | 주문 가능 현금 |
| `tot_evlu_amt` | 총평가금액 | 보유종목 + 예수금 |
| `pchs_amt_smtl_amt` | 매입금액합계 | |
| `evlu_amt_smtl_amt` | 평가금액합계 | |
| `evlu_pfls_smtl_amt` | 평가손익합계 | |
| `nass_amt` | 순자산금액 | |

### JSON 응답 예시

```json
{
  "rt_cd": "0",
  "msg_cd": "MCA00000",
  "msg1": "정상처리 되었습니다.",
  "output1": [
    {
      "pdno": "005930",
      "prdt_name": "삼성전자",
      "hldg_qty": "100",
      "pchs_avg_pric": "70000.0000",
      "pchs_amt": "7000000",
      "prpr": "72500",
      "evlu_amt": "7250000",
      "evlu_pfls_amt": "250000",
      "evlu_pfls_rt": "3.57"
    }
  ],
  "output2": [
    {
      "dnca_tot_amt": "5000000",
      "tot_evlu_amt": "12250000",
      "pchs_amt_smtl_amt": "7000000",
      "evlu_amt_smtl_amt": "7250000",
      "evlu_pfls_smtl_amt": "250000"
    }
  ]
}
```

### Rust 구조체

```rust
/// 개별 종목 잔고
#[derive(Debug, Deserialize)]
pub struct PositionItem {
    pub pdno: String,                  // 종목코드
    pub prdt_name: String,             // 종목명
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub hldg_qty: i64,                 // 보유수량
    #[serde(deserialize_with = "string_to_f64::deserialize")]
    pub pchs_avg_pric: f64,            // 매입평균가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub prpr: i64,                     // 현재가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub evlu_pfls_amt: i64,            // 평가손익
    #[serde(deserialize_with = "string_to_f64::deserialize")]
    pub evlu_pfls_rt: f64,             // 평가손익률 (%)
}

/// 계좌 합계
#[derive(Debug, Deserialize)]
pub struct AccountSummary {
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub dnca_tot_amt: i64,             // 예수금
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub tot_evlu_amt: i64,             // 총평가금액
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    pub evlu_pfls_smtl_amt: i64,       // 평가손익합계
}
```

## 일별 주문체결 내역

### 엔드포인트

```
GET /uapi/domestic-stock/v1/trading/inquire-daily-ccld
```

| 환경 | tr_id | 비고 |
|------|-------|------|
| 실전투자 (3개월 이내) | `TTTC8001R` | |
| 실전투자 (3개월 이전) | `CTSC9115R` | |
| 모의투자 | `VTTC8001R` | |

### 요청 파라미터

| 파라미터 | 타입 | 필수 | 설명 |
|---------|------|------|------|
| `CANO` | String(8) | O | 종합계좌번호 |
| `ACNT_PRDT_CD` | String(2) | O | 계좌상품코드 |
| `INQR_STRT_DT` | String(8) | O | 조회시작일 (YYYYMMDD) |
| `INQR_END_DT` | String(8) | O | 조회종료일 (YYYYMMDD) |
| `SLL_BUY_DVSN_CD` | String(2) | O | `00`=전체, `01`=매도, `02`=매수 |
| `INQR_DVSN` | String(2) | O | `00`=역순, `01`=정순 |
| `PDNO` | String | X | 종목코드 (빈값=전체) |
| `CCLD_DVSN` | String(2) | O | `00`=전체, `01`=체결, `02`=미체결 |

### 응답 주요 필드 (output1 — 배열)

| 필드명 | 설명 |
|--------|------|
| `ord_dt` | 주문일자 |
| `ord_tmd` | 주문시각 |
| `odno` | 주문번호 |
| `sll_buy_dvsn_cd_name` | 매도/매수 구분명 |
| `pdno` | 종목코드 |
| `prdt_name` | 종목명 |
| `ord_qty` | 주문수량 |
| `ord_unpr` | 주문단가 |
| `tot_ccld_qty` | 총체결수량 |
| `avg_prvs` | 체결평균가 |
| `ccld_cndt_name` | 체결상태명 |

## 매수 가능 금액 조회

주문 전에 현재 매수 가능한 금액을 확인한다.

### 엔드포인트

```
GET /uapi/domestic-stock/v1/trading/inquire-psbl-order
```

| 환경 | tr_id |
|------|-------|
| 실전투자 | `TTTC8908R` |
| 모의투자 | `VTTC8908R` |

### 요청 파라미터

| 파라미터 | 타입 | 필수 | 설명 |
|---------|------|------|------|
| `CANO` | String(8) | O | 종합계좌번호 |
| `ACNT_PRDT_CD` | String(2) | O | 계좌상품코드 |
| `PDNO` | String(6) | O | 종목코드 |
| `ORD_UNPR` | String | O | 주문단가 |
| `ORD_DVSN` | String(2) | O | 주문구분 |

### 응답 주요 필드

| 필드명 | 설명 |
|--------|------|
| `ord_psbl_cash` | 주문가능현금 |
| `ord_psbl_qty` | 주문가능수량 |
| `nrcvb_buy_amt` | 미수불가매수금액 |

### 매수 전 검증 패턴

```rust
/// 매수 전 가용 자금 확인
async fn check_buyable(
    client: &KisClient,
    stock_code: &StockCode,
    price: i64,
    quantity: u64,
) -> Result<bool, KisError> {
    let buyable = client.inquire_buyable(stock_code, price).await?;
    let required = price as u64 * quantity;
    Ok(buyable.ord_psbl_cash >= required as i64)
}
```

## 실전/모의투자 차이

| API | 실전투자 | 모의투자 |
|-----|---------|---------|
| 잔고 조회 | `TTTC8434R` | `VTTC8434R` |
| 일별체결 (3개월 이내) | `TTTC8001R` | `VTTC8001R` |
| 일별체결 (3개월 이전) | `CTSC9115R` | 미지원 |
| 매수가능액 | `TTTC8908R` | `VTTC8908R` |

모의투자 계좌는 초기 가상 자금이 설정되어 있으며, KIS 개발자 포털에서 리셋 가능.

## 관련 문서

- [국내주식 주문](domestic-stock-trading.md) — 매수 전 잔고 확인 → 주문 실행
- [에러 처리](error-handling.md) — 잔고 부족 등 에러 처리
- [데이터 타입](data-types.md) — 응답 필드 타입 변환
