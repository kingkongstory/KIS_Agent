use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::application::dto::indicator_dto::IndicatorResultDto;
use crate::application::dto::price_dto::{CandleDto, OrderBookDto, PriceDto};
use crate::application::services::indicator_service::IndicatorService;
use crate::domain::models::candle::Candle;
use crate::domain::ports::market_data::StockSearchResult;
use crate::domain::types::{PeriodCode, StockCode};
use crate::presentation::app_state::AppState;
use crate::presentation::error::AppError;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/stocks/{code}/price", get(get_price))
        .route("/api/v1/stocks/{code}/orderbook", get(get_orderbook))
        .route("/api/v1/stocks/{code}/candles", get(get_candles))
        .route("/api/v1/stocks/{code}/indicators", get(get_indicators))
        .route("/api/v1/stocks/search", get(search_stocks))
}

async fn get_price(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<PriceDto>, AppError> {
    let stock_code = StockCode::new(&code)?;
    let dto = state.market_data.get_price(&stock_code).await?;
    Ok(Json(dto))
}

async fn get_orderbook(
    State(state): State<AppState>,
    Path(code): Path<String>,
) -> Result<Json<OrderBookDto>, AppError> {
    let stock_code = StockCode::new(&code)?;
    let dto = state.market_data.get_orderbook(&stock_code).await?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
struct CandleQuery {
    start: String,
    end: String,
    #[serde(default = "default_period")]
    period: String,
}

fn default_period() -> String {
    "D".to_string()
}

async fn get_candles(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Query(query): Query<CandleQuery>,
) -> Result<Json<Vec<CandleDto>>, AppError> {
    let stock_code = StockCode::new(&code)?;
    let period = match query.period.as_str() {
        "W" => PeriodCode::Week,
        "M" => PeriodCode::Month,
        _ => PeriodCode::Day,
    };
    let dto = state
        .market_data
        .get_candles(&stock_code, &query.start, &query.end, period)
        .await?;
    Ok(Json(dto))
}

#[derive(Deserialize)]
struct IndicatorQuery {
    names: String,
    start: String,
    end: String,
    #[serde(default = "default_period")]
    period: String,
}

async fn get_indicators(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Query(query): Query<IndicatorQuery>,
) -> Result<Json<Vec<IndicatorResultDto>>, AppError> {
    let stock_code = StockCode::new(&code)?;
    let period = match query.period.as_str() {
        "W" => PeriodCode::Week,
        "M" => PeriodCode::Month,
        _ => PeriodCode::Day,
    };

    // 캔들 데이터 조회
    let candle_dtos = state
        .market_data
        .get_candles(&stock_code, &query.start, &query.end, period)
        .await?;

    // DTO → 도메인 Candle 변환
    let candles: Vec<Candle> = candle_dtos
        .into_iter()
        .filter_map(|c| {
            chrono::NaiveDate::parse_from_str(&c.date, "%Y-%m-%d")
                .ok()
                .map(|date| Candle {
                    date,
                    open: c.open,
                    high: c.high,
                    low: c.low,
                    close: c.close,
                    volume: c.volume,
                })
        })
        .collect();

    let names: Vec<String> = query.names.split(',').map(|s| s.trim().to_string()).collect();
    let results = IndicatorService::calculate(&names, &candles);
    Ok(Json(results))
}

#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}

async fn search_stocks(
    State(state): State<AppState>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<StockSearchResult>>, AppError> {
    let results = state.market_data.search_stock(&query.q).await?;
    Ok(Json(results))
}
