> [docs/README.md](../README.md) > [api/](./overview.md) > error-handling.md

# 에러 코드 및 처리

## KIS API 에러 응답 구조

모든 REST API 응답에는 에러 판별 필드가 포함된다:

```json
{
  "rt_cd": "1",
  "msg_cd": "EGW00123",
  "msg1": "기간별시세 조회가 불가합니다."
}
```

| 필드 | 타입 | 설명 |
|------|------|------|
| `rt_cd` | String | `"0"` = 성공, `"0"` 이외 = 실패 |
| `msg_cd` | String | 상세 에러 코드 (예: `EGW00123`) |
| `msg1` | String | 에러 메시지 (한국어) |

## 주요 에러 코드 분류

### 인증 관련

| msg_cd | 설명 | 대응 |
|--------|------|------|
| `EGW00123` | 접근 토큰 만료 | 토큰 재발급 후 재시도 |
| `EGW00121` | 잘못된 앱키 | 앱키 확인 |
| `EGW00122` | 잘못된 앱시크릿 | 앱시크릿 확인 |
| `EGW00201` | 토큰 발급 횟수 초과 (분당 1회) | 1분 대기 후 재시도 |

### 주문 관련

| msg_cd | 설명 | 대응 |
|--------|------|------|
| `APBK0919` | 매수 가능 금액 부족 | 잔고 확인 |
| `APBK0634` | 주문 수량 오류 | 수량 검증 |
| `APBK0500` | 호가 단위 오류 | 호가 단위표 참조 |
| `APBK1001` | 장 운영시간 외 | 시간 확인 |
| `APBK0700` | 해당 종목 없음 | 종목코드 확인 |

### 시세 조회 관련

| msg_cd | 설명 | 대응 |
|--------|------|------|
| `MKSC0001` | 존재하지 않는 종목 | 종목코드 확인 |
| `MKSC0003` | 조회 제한 초과 | 속도 제한 대응 |

### 속도 제한

| msg_cd | 설명 | 대응 |
|--------|------|------|
| `EGW00200` | 초당 호출 제한 초과 | 슬라이딩 윈도우 대기 |

## Rust 에러 타입 설계

### KisError enum

```rust
use thiserror::Error;

#[derive(Error, Debug)]
pub enum KisError {
    // --- 인증 ---
    #[error("토큰 만료: {0}")]
    TokenExpired(String),

    #[error("인증 실패: {0}")]
    AuthenticationFailed(String),

    #[error("토큰 발급 제한 초과 (분당 1회)")]
    TokenRateLimited,

    // --- 주문 ---
    #[error("잔고 부족: {0}")]
    InsufficientBalance(String),

    #[error("주문 유효성 오류: {0}")]
    OrderValidation(String),

    #[error("장 운영시간 외")]
    MarketClosed,

    // --- 시세 ---
    #[error("존재하지 않는 종목: {0}")]
    InvalidStockCode(String),

    // --- 속도 제한 ---
    #[error("API 호출 제한 초과")]
    RateLimited,

    // --- 네트워크 ---
    #[error("HTTP 요청 실패: {0}")]
    HttpError(#[from] reqwest::Error),

    #[error("JSON 파싱 실패: {0}")]
    ParseError(#[from] serde_json::Error),

    // --- KIS API 일반 에러 ---
    #[error("KIS API 에러 [{msg_cd}]: {msg}")]
    ApiError {
        rt_cd: String,
        msg_cd: String,
        msg: String,
    },
}
```

### 재시도 가능 여부 판단

```rust
impl KisError {
    /// 재시도할 수 있는 에러인지 판단
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            KisError::TokenExpired(_)
                | KisError::RateLimited
                | KisError::TokenRateLimited
                | KisError::HttpError(_)
        )
    }

    /// 재시도 전 대기 시간 (밀리초)
    pub fn retry_delay_ms(&self) -> u64 {
        match self {
            KisError::RateLimited => 1000,        // 1초
            KisError::TokenRateLimited => 60_000,  // 1분
            KisError::TokenExpired(_) => 0,        // 즉시 토큰 갱신
            KisError::HttpError(_) => 3000,        // 3초 (네트워크 복구 대기)
            _ => 0,
        }
    }
}
```

### 응답 파싱 및 에러 변환

```rust
/// KIS API 공통 응답 래퍼
#[derive(Deserialize)]
struct KisResponse<T> {
    rt_cd: String,
    msg_cd: String,
    msg1: String,
    #[serde(default)]
    output: Option<T>,
    #[serde(default)]
    output1: Option<T>,
}

impl<T> KisResponse<T> {
    fn into_result(self) -> Result<T, KisError> {
        if self.rt_cd == "0" {
            self.output
                .or(self.output1)
                .ok_or(KisError::ApiError {
                    rt_cd: self.rt_cd,
                    msg_cd: self.msg_cd,
                    msg: "응답에 output이 없습니다".to_string(),
                })
        } else {
            Err(self.classify_error())
        }
    }

    fn classify_error(self) -> KisError {
        match self.msg_cd.as_str() {
            "EGW00123" => KisError::TokenExpired(self.msg1),
            "EGW00121" | "EGW00122" => KisError::AuthenticationFailed(self.msg1),
            "EGW00201" => KisError::TokenRateLimited,
            "EGW00200" => KisError::RateLimited,
            "APBK0919" => KisError::InsufficientBalance(self.msg1),
            "APBK1001" => KisError::MarketClosed,
            "MKSC0001" | "APBK0700" => KisError::InvalidStockCode(self.msg1),
            _ => KisError::ApiError {
                rt_cd: self.rt_cd,
                msg_cd: self.msg_cd,
                msg: self.msg1,
            },
        }
    }
}
```

## 에러 처리 모범 사례

### 1. 토큰 만료 자동 복구

```rust
async fn api_call_with_retry<T>(/* ... */) -> Result<T, KisError> {
    let mut last_error = None;
    for attempt in 0..3 {
        match execute_api_call().await {
            Ok(result) => return Ok(result),
            Err(e) if e.is_retryable() => {
                if matches!(e, KisError::TokenExpired(_)) {
                    token_manager.refresh_token().await?;
                }
                let delay = e.retry_delay_ms();
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                last_error = Some(e);
            }
            Err(e) => return Err(e), // 재시도 불가능한 에러
        }
    }
    Err(last_error.unwrap())
}
```

### 2. 로깅

- 재시도 가능한 에러: `warn!` 레벨
- 치명적 에러 (인증 실패, 잘못된 종목코드): `error!` 레벨
- 성공 응답: `debug!` 레벨

## 관련 문서

- [API 공통사항](overview.md) — 공통 응답 형식
- [속도 제한](rate-limits.md) — 속도 제한 에러 상세
- [인증](authentication.md) — 토큰 만료 처리
