use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::application::dto::account_dto::{BalanceDto, BuyableDto, ExecutionDto};
use crate::domain::types::StockCode;
use crate::presentation::app_state::AppState;
use crate::presentation::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/account/balance", get(get_balance))
        .route("/api/v1/account/executions", get(get_executions))
        .route("/api/v1/account/buyable", get(get_buyable))
}

async fn get_balance(
    State(state): State<AppState>,
) -> Result<Json<BalanceDto>, AppError> {
    let dto = state.account.get_balance().await?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
struct ExecutionQuery {
    start: String,
    end: String,
}

async fn get_executions(
    State(state): State<AppState>,
    Query(query): Query<ExecutionQuery>,
) -> Result<Json<Vec<ExecutionDto>>, AppError> {
    let dto = state
        .account
        .get_executions(&query.start, &query.end)
        .await?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
struct BuyableQuery {
    stock_code: String,
    price: i64,
}

async fn get_buyable(
    State(state): State<AppState>,
    Query(query): Query<BuyableQuery>,
) -> Result<Json<BuyableDto>, AppError> {
    let stock_code = StockCode::new(&query.stock_code)?;
    let dto = state
        .account
        .get_buyable(&stock_code, query.price)
        .await?;
    Ok(Json(dto))
}
