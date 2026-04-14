pub mod account;
pub mod health;
pub mod market_data;
pub mod monitoring;
pub mod strategy;
pub mod trading;

use axum::Router;

use super::app_state::AppState;

/// 전체 라우터 구성
pub fn create_router(state: AppState) -> Router {
    Router::new()
        .merge(health::routes())
        .merge(market_data::routes())
        .merge(trading::routes())
        .merge(account::routes())
        .merge(strategy::routes())
        .merge(monitoring::routes())
        .merge(super::ws::routes())
        .with_state(state)
}
