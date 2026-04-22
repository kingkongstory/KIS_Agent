use axum::extract::{Path, State};
use axum::routing::{delete, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use crate::application::dto::order_dto::{OrderRequestDto, OrderResponseDto};
use crate::domain::models::order::{CancelOrderRequest, ModifyOrderRequest, OrderType};
use crate::presentation::app_state::AppState;
use crate::presentation::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/orders", post(place_order))
        .route("/api/v1/orders/{no}/modify", put(modify_order))
        .route("/api/v1/orders/{no}", delete(cancel_order))
}

async fn place_order(
    State(state): State<AppState>,
    Json(dto): Json<OrderRequestDto>,
) -> Result<Json<OrderResponseDto>, AppError> {
    let request = dto
        .into_domain()
        .map_err(|e| AppError(crate::domain::error::KisError::OrderValidation(e)))?;
    let response = state.trading.place_order(request).await?;
    Ok(Json(response))
}

#[derive(Deserialize)]
struct ModifyBody {
    original_krx_orgno: String,
    order_type: String,
    quantity: u64,
    price: i64,
}

async fn modify_order(
    State(state): State<AppState>,
    Path(order_no): Path<String>,
    Json(body): Json<ModifyBody>,
) -> Result<Json<OrderResponseDto>, AppError> {
    let order_type = match body.order_type.as_str() {
        "limit" => OrderType::Limit,
        "market" => OrderType::Market,
        _ => {
            return Err(AppError(crate::domain::error::KisError::OrderValidation(
                format!("잘못된 주문 유형: {}", body.order_type),
            )));
        }
    };

    let request = ModifyOrderRequest {
        original_order_no: order_no,
        original_krx_orgno: body.original_krx_orgno,
        order_type,
        quantity: body.quantity,
        price: body.price,
    };
    let response = state.trading.modify_order(request).await?;
    Ok(Json(response))
}

#[derive(Deserialize)]
struct CancelQuery {
    #[serde(default)]
    original_krx_orgno: String,
    #[serde(default)]
    quantity: u64,
}

async fn cancel_order(
    State(state): State<AppState>,
    Path(order_no): Path<String>,
    axum::extract::Query(query): axum::extract::Query<CancelQuery>,
) -> Result<Json<OrderResponseDto>, AppError> {
    let request = CancelOrderRequest {
        original_order_no: order_no,
        original_krx_orgno: query.original_krx_orgno,
        quantity: query.quantity,
    };
    let response = state.trading.cancel_order(request).await?;
    Ok(Json(response))
}
