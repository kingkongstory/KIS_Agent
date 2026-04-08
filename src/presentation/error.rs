use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::domain::error::KisError;

/// 프레젠테이션 레이어 에러 → HTTP 응답
pub struct AppError(pub KisError);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self.0 {
            KisError::InvalidStockCode(_) => (StatusCode::BAD_REQUEST, self.0.to_string()),
            KisError::OrderValidation(_) => (StatusCode::BAD_REQUEST, self.0.to_string()),
            KisError::InsufficientBalance(_) => (StatusCode::UNPROCESSABLE_ENTITY, self.0.to_string()),
            KisError::MarketClosed => (StatusCode::SERVICE_UNAVAILABLE, self.0.to_string()),
            KisError::RateLimited | KisError::TokenRateLimited => {
                (StatusCode::TOO_MANY_REQUESTS, self.0.to_string())
            }
            KisError::AuthenticationFailed(_) | KisError::TokenExpired(_) => {
                (StatusCode::UNAUTHORIZED, self.0.to_string())
            }
            _ => (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()),
        };

        let body = json!({
            "error": message,
            "code": status.as_u16(),
        });

        (status, axum::Json(body)).into_response()
    }
}

impl From<KisError> for AppError {
    fn from(e: KisError) -> Self {
        Self(e)
    }
}
