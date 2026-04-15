use thiserror::Error;

/// KIS API 에러 타입
#[derive(Error, Debug)]
pub enum KisError {
    /// 토큰 만료
    #[error("토큰 만료: {0}")]
    TokenExpired(String),

    /// 인증 실패
    #[error("인증 실패: {0}")]
    AuthenticationFailed(String),

    /// 토큰 발급 제한 초과 (분당 1회)
    #[error("토큰 발급 제한 초과 (분당 1회)")]
    TokenRateLimited,

    /// 잔고 부족
    #[error("잔고 부족: {0}")]
    InsufficientBalance(String),

    /// 주문 유효성 오류
    #[error("주문 유효성 오류: {0}")]
    OrderValidation(String),

    /// 장 운영시간 외
    #[error("장 운영시간 외")]
    MarketClosed,

    /// 존재하지 않는 종목
    #[error("존재하지 않는 종목: {0}")]
    InvalidStockCode(String),

    /// API 호출 제한 초과
    #[error("API 호출 제한 초과")]
    RateLimited,

    /// HTTP 요청 실패
    #[error("HTTP 요청 실패: {0}")]
    HttpError(String),

    /// JSON 파싱 실패
    #[error("JSON 파싱 실패: {0}")]
    ParseError(String),

    /// WebSocket 에러
    #[error("WebSocket 에러: {0}")]
    WebSocketError(String),

    /// KIS API 에러 (분류되지 않은 에러)
    #[error("KIS API 에러 [{msg_cd}]: {msg}")]
    ApiError {
        rt_cd: String,
        msg_cd: String,
        msg: String,
    },

    /// 내부 에러
    #[error("내부 에러: {0}")]
    Internal(String),

    /// 다른 종목 선점 요청으로 진입 포기 (dual-locked 양보)
    ///
    /// FVG 자체는 유효하며 search_after 전진은 부적절.
    /// 단순히 이번 사이클 lock 경쟁에 양보하는 의미.
    #[error("다른 종목 선점 요청으로 진입 포기")]
    Preempted,

    /// 수동 개입 필요 — 상위에서 일반 진입 실패와 분리 처리해야 함.
    ///
    /// 2026-04-15 Codex review #4 대응. cancel_verify_failed / safe_orphan_cleanup 의
    /// 보유 감지 경로 등 "상태 불명 또는 DB 메타 부재" 상황에서 사용.
    /// 상위(`poll_and_enter`)는 이 variant 수신 시 `abort_entry` 를 호출하지 않고
    /// shared `position_lock` 을 그대로 유지하여 다른 종목 진입을 차단한다.
    #[error("수동 개입 필요: {0}")]
    ManualIntervention(String),
}

impl KisError {
    /// 재시도 가능 여부 판별
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            KisError::TokenExpired(_)
                | KisError::RateLimited
                | KisError::HttpError(_)
                | KisError::WebSocketError(_)
        )
    }

    /// 재시도 대기 시간 (밀리초)
    pub fn retry_delay_ms(&self) -> u64 {
        match self {
            KisError::RateLimited => 1_000,
            KisError::TokenRateLimited => 60_000,
            KisError::TokenExpired(_) => 100,
            KisError::HttpError(_) => 500,
            KisError::WebSocketError(_) => 1_000,
            _ => 0,
        }
    }

    /// msg_cd 기반 에러 분류
    pub fn classify(rt_cd: String, msg_cd: String, msg: String) -> Self {
        match msg_cd.as_str() {
            "EGW00123" => KisError::TokenExpired(msg),
            "EGW00121" | "EGW00122" => KisError::AuthenticationFailed(msg),
            "EGW00201" => KisError::RateLimited, // 초당 거래건수 초과
            "EGW00200" => KisError::RateLimited,
            "APBK0919" => KisError::InsufficientBalance(msg),
            "APBK1001" => KisError::MarketClosed,
            "MKSC0001" | "APBK0700" => KisError::InvalidStockCode(msg),
            _ => KisError::ApiError { rt_cd, msg_cd, msg },
        }
    }
}

impl From<serde_json::Error> for KisError {
    fn from(e: serde_json::Error) -> Self {
        KisError::ParseError(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_token_expired() {
        let err = KisError::classify(
            "1".into(),
            "EGW00123".into(),
            "기간이 만료된 token".into(),
        );
        assert!(matches!(err, KisError::TokenExpired(_)));
        assert!(err.is_retryable());
    }

    #[test]
    fn test_classify_rate_limited() {
        let err = KisError::classify("1".into(), "EGW00200".into(), "초과".into());
        assert!(matches!(err, KisError::RateLimited));
        assert_eq!(err.retry_delay_ms(), 1_000);
    }

    #[test]
    fn test_classify_unknown() {
        let err = KisError::classify("1".into(), "UNKNOWN".into(), "msg".into());
        assert!(matches!(err, KisError::ApiError { .. }));
        assert!(!err.is_retryable());
    }

    #[test]
    fn test_order_validation_not_retryable() {
        let err = KisError::OrderValidation("가격 오류".into());
        assert!(!err.is_retryable());
        assert_eq!(err.retry_delay_ms(), 0);
    }
}
