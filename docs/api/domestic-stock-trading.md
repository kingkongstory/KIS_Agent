> [docs/README.md](../README.md) > [api/](./overview.md) > domestic-stock-trading.md

# 국내주식 주문

## 주문 흐름

```
주문 요청 → 접수 → 체결 (전량/부분) 또는 미체결
                  → 정정/취소 가능 (미체결분)
```

## 현금 매수 주문

### 엔드포인트

```
POST /uapi/domestic-stock/v1/trading/order-cash
```

| 환경 | tr_id |
|------|-------|
| 실전투자 | `TTTC0802U` |
| 모의투자 | `VTTC0802U` |

### 요청 헤더

공통 헤더([overview.md](overview.md)) + `tr_id` 설정

### 요청 본문

| 필드 | 타입 | 필수 | 설명 |
|------|------|------|------|
| `CANO` | String(8) | O | 종합계좌번호 (8자리) |
| `ACNT_PRDT_CD` | String(2) | O | 계좌상품코드 (`01`=종합) |
| `PDNO` | String(6) | O | 종목코드 |
| `ORD_DVSN` | String(2) | O | 주문구분 (아래 표 참조) |
| `ORD_QTY` | String | O | 주문수량 |
| `ORD_UNPR` | String | O | 주문단가 (시장가 시 `"0"`) |

### 주문구분 코드 (ORD_DVSN)

| 코드 | 구분 | 설명 |
|------|------|------|
| `00` | 지정가 | 지정한 가격으로 주문 |
| `01` | 시장가 | 시장 가격으로 즉시 체결 |
| `02` | 조건부지정가 | 장 중 지정가, 장 마감 시 시장가로 전환 |
| `03` | 최유리지정가 | 상대측 최우선 호가로 주문 |
| `04` | 최우선지정가 | 자기측 최우선 호가로 주문 |
| `05` | 장전 시간외 | 장 시작 전 (08:30~08:40) |
| `06` | 장후 시간외 | 장 마감 후 (15:40~16:00) |
| `07` | 시간외 단일가 | 시간외 단일가매매 (16:00~18:00) |

> **모의투자 제한**: 모의투자에서는 `00`(지정가)과 `01`(시장가)만 지원되는 경우가 많다.

### JSON 요청 예시 (삼성전자 지정가 매수)

```json
{
  "CANO": "12345678",
  "ACNT_PRDT_CD": "01",
  "PDNO": "005930",
  "ORD_DVSN": "00",
  "ORD_QTY": "10",
  "ORD_UNPR": "72500"
}
```

### 응답

```json
{
  "rt_cd": "0",
  "msg_cd": "APBK0013",
  "msg1": "주문 전송 완료 되었습니다.",
  "output": {
    "KRX_FWDG_ORD_ORGNO": "91252",
    "ODNO": "0000123456",
    "ORD_TMD": "103025"
  }
}
```

| 필드 | 설명 |
|------|------|
| `KRX_FWDG_ORD_ORGNO` | 거래소 전송 주문 조직번호 |
| `ODNO` | 주문번호 (정정/취소 시 필요) |
| `ORD_TMD` | 주문시각 (HHMMSS) |

## 현금 매도 주문

매수와 동일한 엔드포인트, 다른 tr_id를 사용한다.

| 환경 | tr_id |
|------|-------|
| 실전투자 | `TTTC0801U` |
| 모의투자 | `VTTC0801U` |

요청 본문 구조는 매수와 동일하다. 보유 수량 이하로만 매도 가능.

## 주문 정정

미체결 주문의 가격 또는 수량을 변경한다.

### 엔드포인트

```
POST /uapi/domestic-stock/v1/trading/order-rvsecncl
```

| 환경 | tr_id |
|------|-------|
| 실전투자 | `TTTC0803U` |
| 모의투자 | `VTTC0803U` |

### 요청 본문

| 필드 | 타입 | 필수 | 설명 |
|------|------|------|------|
| `CANO` | String(8) | O | 종합계좌번호 |
| `ACNT_PRDT_CD` | String(2) | O | 계좌상품코드 |
| `KRX_FWDG_ORD_ORGNO` | String | O | 원주문 거래소번호 |
| `ORGN_ODNO` | String | O | 원주문번호 |
| `ORD_DVSN` | String(2) | O | 주문구분 |
| `RVSE_CNCL_DVSN_CD` | String(2) | O | `01`=정정, `02`=취소 |
| `ORD_QTY` | String | O | 정정/취소 수량 |
| `ORD_UNPR` | String | O | 정정 가격 (취소 시 `"0"`) |
| `QTY_ALL_ORD_YN` | String(1) | O | `Y`=전량, `N`=일부 |

## 주문 취소

정정과 동일한 엔드포인트에서 `RVSE_CNCL_DVSN_CD`를 `"02"`로 설정한다.

```json
{
  "CANO": "12345678",
  "ACNT_PRDT_CD": "01",
  "KRX_FWDG_ORD_ORGNO": "91252",
  "ORGN_ODNO": "0000123456",
  "ORD_DVSN": "00",
  "RVSE_CNCL_DVSN_CD": "02",
  "ORD_QTY": "0",
  "ORD_UNPR": "0",
  "QTY_ALL_ORD_YN": "Y"
}
```

## 호가 단위 규칙

주문 가격은 가격대별 호가 단위에 맞아야 한다:

| 가격대 | 호가 단위 |
|--------|----------|
| 2,000원 미만 | 1원 |
| 2,000원 이상 ~ 5,000원 미만 | 5원 |
| 5,000원 이상 ~ 20,000원 미만 | 10원 |
| 20,000원 이상 ~ 50,000원 미만 | 50원 |
| 50,000원 이상 ~ 200,000원 미만 | 100원 |
| 200,000원 이상 ~ 500,000원 미만 | 500원 |
| 500,000원 이상 | 1,000원 |

```rust
/// 가격대별 호가 단위를 반환
fn tick_size(price: i64) -> i64 {
    match price {
        p if p < 2_000 => 1,
        p if p < 5_000 => 5,
        p if p < 20_000 => 10,
        p if p < 50_000 => 50,
        p if p < 200_000 => 100,
        p if p < 500_000 => 500,
        _ => 1_000,
    }
}

/// 호가 단위에 맞게 가격을 조정 (내림)
fn align_price(price: i64) -> i64 {
    let tick = tick_size(price);
    (price / tick) * tick
}
```

## 장 운영 시간

| 구분 | 시간 | 비고 |
|------|------|------|
| 정규장 | 09:00 ~ 15:30 | 일반 거래 |
| 장전 시간외 | 08:30 ~ 08:40 | 전일 종가 기준 |
| 장후 시간외 | 15:40 ~ 16:00 | 당일 종가 기준 |
| 시간외 단일가 | 16:00 ~ 18:00 | 10분 단위 체결 |

## Rust 구현 가이드

```rust
/// 주문 유형
enum OrderSide {
    Buy,
    Sell,
}

/// 주문 요청
struct OrderRequest {
    account_no: String,       // CANO
    product_code: String,     // ACNT_PRDT_CD
    stock_code: StockCode,    // PDNO
    order_type: OrderType,    // ORD_DVSN
    quantity: u64,            // ORD_QTY
    price: i64,               // ORD_UNPR (시장가 시 0)
}

/// 주문 응답
struct OrderResponse {
    order_no: String,         // ODNO
    org_no: String,           // KRX_FWDG_ORD_ORGNO
    order_time: String,       // ORD_TMD
}

/// 주문 전 검증
impl OrderRequest {
    fn validate(&self) -> Result<(), KisError> {
        // 호가 단위 검증
        if self.price > 0 {
            let tick = tick_size(self.price);
            if self.price % tick != 0 {
                return Err(KisError::OrderValidation(
                    format!("호가 단위 불일치: {}원 (단위: {}원)", self.price, tick)
                ));
            }
        }
        // 수량 검증
        if self.quantity == 0 {
            return Err(KisError::OrderValidation("수량은 1 이상이어야 합니다".into()));
        }
        Ok(())
    }
}
```

## 실전/모의투자 차이

| 항목 | 실전투자 | 모의투자 |
|------|---------|---------|
| 매수 tr_id | `TTTC0802U` | `VTTC0802U` |
| 매도 tr_id | `TTTC0801U` | `VTTC0801U` |
| 정정/취소 tr_id | `TTTC0803U` | `VTTC0803U` |
| 주문구분 | 전체 지원 | 지정가/시장가만 |
| 체결 속도 | 실제 시장 | 시뮬레이션 (지연 있을 수 있음) |

## 관련 문서

- [계좌 조회](account.md) — 매수 전 잔고/매수가능액 확인
- [에러 처리](error-handling.md) — 주문 실패 에러 코드
- [속도 제한](rate-limits.md) — 주문 요청 제한
- [지표 조합 전략](../indicators/strategies.md) — 시그널 → 주문 실행 연동
