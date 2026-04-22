use crate::application::dto::indicator_dto::IndicatorResultDto;
use crate::domain::indicators::engine::IndicatorEngine;
use crate::domain::models::candle::Candle;

/// 지표 서비스
pub struct IndicatorService;

impl IndicatorService {
    /// 캔들 데이터로 지표 계산
    pub fn calculate(names: &[String], candles: &[Candle]) -> Vec<IndicatorResultDto> {
        let results = IndicatorEngine::calculate_all(names, candles);
        results
            .into_iter()
            .filter_map(|(name, result)| {
                result
                    .ok()
                    .map(|values| IndicatorResultDto::from_values(name, values))
            })
            .collect()
    }
}
