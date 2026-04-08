use std::collections::HashMap;

use serde::Serialize;

use crate::domain::indicators::traits::IndicatorValue;

/// 지표 결과 DTO
#[derive(Debug, Serialize)]
pub struct IndicatorResultDto {
    pub name: String,
    pub values: Vec<IndicatorPointDto>,
}

#[derive(Debug, Serialize)]
pub struct IndicatorPointDto {
    pub date: String,
    pub values: HashMap<String, f64>,
}

impl IndicatorResultDto {
    pub fn from_values(name: String, values: Vec<IndicatorValue>) -> Self {
        Self {
            name,
            values: values
                .into_iter()
                .map(|v| IndicatorPointDto {
                    date: v.date.format("%Y-%m-%d").to_string(),
                    values: v.values,
                })
                .collect(),
        }
    }
}
