use std::collections::HashMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::domain::models::candle::Candle;

/// 매매 시그널
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Signal {
    StrongBuy,
    Buy,
    Hold,
    Sell,
    StrongSell,
}

/// 지표 계산 결과
#[derive(Debug, Clone, Serialize)]
pub struct IndicatorValue {
    pub date: NaiveDate,
    pub values: HashMap<String, f64>,
    pub signal: Signal,
}

/// 지표 에러
#[derive(Debug, thiserror::Error)]
pub enum IndicatorError {
    #[error("데이터 부족: 최소 {required}개 필요, {actual}개 제공")]
    InsufficientData { required: usize, actual: usize },

    #[error("잘못된 파라미터: {0}")]
    InvalidParameter(String),
}

/// 기술적 지표 공통 인터페이스
pub trait Indicator: Send + Sync {
    /// 지표 이름
    fn name(&self) -> &str;

    /// 최소 필요 캔들 수
    fn required_candles(&self) -> usize;

    /// 배치 계산
    fn calculate(&self, candles: &[Candle]) -> Result<Vec<IndicatorValue>, IndicatorError>;

    /// 증분 업데이트
    fn update(&mut self, candle: &Candle) -> Result<Option<IndicatorValue>, IndicatorError>;
}
