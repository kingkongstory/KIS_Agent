use std::sync::Arc;
use tokio::sync::broadcast;

use crate::application::services::account_service::AccountService;
use crate::application::services::market_data_service::MarketDataService;
use crate::application::services::trading_service::TradingService;
use crate::domain::ports::realtime::RealtimeData;

/// 애플리케이션 상태 (모든 서비스를 Arc로 공유)
#[derive(Clone)]
pub struct AppState {
    pub market_data: Arc<MarketDataService>,
    pub trading: Arc<TradingService>,
    pub account: Arc<AccountService>,
    pub realtime_tx: broadcast::Sender<RealtimeData>,
}
