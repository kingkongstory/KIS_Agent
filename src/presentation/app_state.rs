use std::sync::Arc;
use sqlx::postgres::PgPool;
use tokio::sync::broadcast;

use crate::application::services::account_service::AccountService;
use crate::application::services::market_data_service::MarketDataService;
use crate::application::services::trading_service::TradingService;
use crate::domain::ports::realtime::RealtimeData;
use crate::infrastructure::websocket::connection::KisWebSocketClient;
use crate::presentation::routes::strategy::StrategyManager;

/// 애플리케이션 상태 (모든 서비스를 Arc로 공유)
#[derive(Clone)]
pub struct AppState {
    pub market_data: Arc<MarketDataService>,
    pub trading: Arc<TradingService>,
    pub account: Arc<AccountService>,
    pub realtime_tx: broadcast::Sender<RealtimeData>,
    pub strategy_manager: StrategyManager,
    pub db_pool: Option<PgPool>,
    /// WS 클라이언트 핸들 — `/api/v1/monitoring/health` 에서 tick/메시지 age 노출용.
    /// 2026-04-16 Task 2.
    pub ws_client: Option<Arc<KisWebSocketClient>>,
}
