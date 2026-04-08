> [docs/README.md](../README.md) > api/ > overview.md

# KIS API 공통사항

## 기본 URL

| 환경 | REST API | WebSocket |
|------|----------|-----------|
| 실전투자 | `https://openapi.koreainvestment.com:9443` | `ws://ops.koreainvestment.com:21000` |
| 모의투자 | `https://openapivts.koreainvestment.com:29443` | `ws://ops.koreainvestment.com:31000` |

Rust에서는 환경을 enum으로 관리한다:

```rust
enum Environment {
    Real,  // 실전투자
    Paper, // 모의투자
}

impl Environment {
    fn rest_base_url(&self) -> &str {
        match self {
            Self::Real => "https://openapi.koreainvestment.com:9443",
            Self::Paper => "https://openapivts.koreainvestment.com:29443",
        }
    }

    fn ws_url(&self) -> &str {
        match self {
            Self::Real => "ws://ops.koreainvestment.com:21000",
            Self::Paper => "ws://ops.koreainvestment.com:31000",
        }
    }
}
```

## 공통 요청 헤더

모든 REST API 요청에 필요한 헤더:

| 헤더 | 값 | 설명 |
|------|-----|------|
| `Content-Type` | `application/json` | 고정값 |
| `authorization` | `Bearer {access_token}` | 인증 토큰 ([인증](authentication.md) 참조) |
| `appkey` | `{appkey}` | KIS 개발자 포털에서 발급받은 앱키 |
| `appsecret` | `{appsecret}` | KIS 개발자 포털에서 발급받은 앱시크릿 |
| `tr_id` | `{transaction_id}` | API 오퍼레이션 식별자 (아래 참조) |
| `custtype` | `P` | 고객 유형 (P = 개인) |

> **주의**: `appkey`와 `appsecret`은 토큰 발급뿐 아니라 **모든 요청에 함께 전송**해야 한다.

## tr_id 체계

`tr_id`는 각 API 오퍼레이션을 식별하는 코드이다. **실전투자와 모의투자에서 동일 기능이라도 코드가 다르다.**

### 국내주식 주요 tr_id 대조표

| 기능 | 실전투자 | 모의투자 | 메서드 |
|------|---------|---------|--------|
| 현금 매수 | `TTTC0802U` | `VTTC0802U` | POST |
| 현금 매도 | `TTTC0801U` | `VTTC0801U` | POST |
| 주문 정정 | `TTTC0803U` | `VTTC0803U` | POST |
| 주문 취소 | `TTTC0803U` | `VTTC0803U` | POST |
| 잔고 조회 | `TTTC8434R` | `VTTC8434R` | GET |
| 일별 체결 | `TTTC8001R` | `VTTC8001R` | GET |
| 매수가능액 | `TTTC8908R` | `VTTC8908R` | GET |

### 시세 조회 tr_id (실전/모의 동일)

| 기능 | tr_id | 메서드 |
|------|-------|--------|
| 현재가 | `FHKST01010100` | GET |
| 호가 | `FHKST01010200` | GET |
| 기간별 시세 | `FHKST03010100` | GET |

### WebSocket tr_id

| 기능 | tr_id |
|------|-------|
| 실시간 체결가 | `H0STCNT0` |
| 실시간 호가 | `H0STASP0` |
| 체결 통보 | `H0STCNI0` / `H0STCNI9` |

Rust에서는 tr_id를 enum으로 관리하여 환경에 따라 자동 변환한다:

```rust
enum TransactionId {
    // 주문
    BuyCash,
    SellCash,
    ModifyCancel,
    // 조회
    InquireBalance,
    InquireDailyCcld,
    InquireBuyable,
    // 시세 (실전/모의 동일)
    InquirePrice,
    InquireAskingPrice,
    InquireDailyPrice,
}

impl TransactionId {
    fn code(&self, env: &Environment) -> &str {
        match (self, env) {
            (Self::BuyCash, Environment::Real) => "TTTC0802U",
            (Self::BuyCash, Environment::Paper) => "VTTC0802U",
            (Self::SellCash, Environment::Real) => "TTTC0801U",
            (Self::SellCash, Environment::Paper) => "VTTC0801U",
            // 시세 조회는 환경 무관
            (Self::InquirePrice, _) => "FHKST01010100",
            // ...
        }
    }
}
```

## 공통 응답 형식

모든 REST API 응답은 아래 공통 필드를 포함한다:

```json
{
  "rt_cd": "0",
  "msg_cd": "MCA00000",
  "msg1": "정상처리 되었습니다.",
  "output": { ... }
}
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `rt_cd` | String | 응답 코드. `"0"` = 성공, 그 외 = 실패 |
| `msg_cd` | String | 메시지 코드 (상세 에러 식별용) |
| `msg1` | String | 사람이 읽을 수 있는 메시지 |
| `output` | Object/Array | 실제 데이터 (API마다 다름) |

일부 API는 `output1`, `output2`로 다중 결과를 반환한다 (예: 잔고 조회 — `output1`: 종목별 잔고, `output2`: 합계).

> 에러 처리 상세는 [에러 처리](error-handling.md) 참조

## Hashkey

POST 요청 시 요청 본문의 무결성 검증을 위한 해시값이다.

- **엔드포인트**: `POST /uapi/hashkey`
- **용도**: 요청 본문을 해시하여 `hashkey` 헤더에 포함
- **현재 상태**: 필수 아님 (선택사항)
- **향후**: 필수화 가능성 있으므로 구현해두는 것을 권장

```json
// 요청
POST /uapi/hashkey
{
  "appkey": "{appkey}",
  "appsecret": "{appsecret}"
}
// + 해시할 요청 본문

// 응답
{
  "HASH": "해시값..."
}
```

## 관련 문서

- [인증](authentication.md) — 토큰 발급 및 관리
- [에러 처리](error-handling.md) — 에러 코드 상세
- [데이터 타입](data-types.md) — KIS 응답의 타입 매핑
- [속도 제한](rate-limits.md) — 호출 제한 규칙
