use chrono::NaiveDate;
use std::sync::Arc;
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::domain::models::candle::Candle;
use crate::infrastructure::cache::postgres_store::{IndexCandle, PostgresStore};

const NAVER_CHART_URL: &str = "https://fchart.stock.naver.com/siseJson.naver";

/// 네이버 금융 일봉 수집기
pub struct NaverDailyCollector {
    client: reqwest::Client,
    store: Arc<PostgresStore>,
}

impl NaverDailyCollector {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self {
            client: reqwest::Client::new(),
            store,
        }
    }

    /// 종목 일봉 OHLCV 수집 및 저장
    pub async fn collect_stock_daily(
        &self,
        stock_code: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<usize, KisError> {
        let candles = self.fetch_daily(stock_code, start_date, end_date).await?;
        if candles.is_empty() {
            warn!("네이버 금융: {stock_code} 데이터 없음 ({start_date}~{end_date})");
            return Ok(0);
        }
        let count = self.store.save_daily_ohlcv(stock_code, &candles).await?;
        info!("네이버 금융: {stock_code} 일봉 {count}건 저장 완료");
        Ok(count)
    }

    /// 지수 일봉 수집 및 저장 (KOSPI, KOSDAQ)
    pub async fn collect_index_daily(
        &self,
        symbol: &str,
        index_code: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<usize, KisError> {
        let candles = self
            .fetch_index_daily(symbol, start_date, end_date)
            .await?;
        if candles.is_empty() {
            warn!("네이버 금융: {symbol} 지수 데이터 없음");
            return Ok(0);
        }
        let count = self.store.save_index_daily(index_code, &candles).await?;
        info!("네이버 금융: {symbol}({index_code}) 지수 일봉 {count}건 저장 완료");
        Ok(count)
    }

    /// 여러 종목 일괄 수집
    pub async fn collect_stocks_batch(
        &self,
        stock_codes: &[&str],
        start_date: &str,
        end_date: &str,
    ) -> Result<usize, KisError> {
        let mut total = 0;
        for (i, code) in stock_codes.iter().enumerate() {
            match self.collect_stock_daily(code, start_date, end_date).await {
                Ok(count) => total += count,
                Err(e) => warn!("네이버 금융: {code} 수집 실패: {e}"),
            }
            // 과도한 요청 방지
            if i > 0 && i % 10 == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
        info!("네이버 금융: {}/{}종목 일봉 수집 완료, 총 {total}건", stock_codes.len(), stock_codes.len());
        Ok(total)
    }

    /// 코스피 + 코스닥 지수 수집
    pub async fn collect_all_indices(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<usize, KisError> {
        let mut total = 0;
        total += self
            .collect_index_daily("KOSPI", "KOSPI", start_date, end_date)
            .await?;
        total += self
            .collect_index_daily("KOSDAQ", "KOSDAQ", start_date, end_date)
            .await?;
        Ok(total)
    }

    /// 네이버 금융 API에서 일봉 데이터 가져오기
    async fn fetch_daily(
        &self,
        symbol: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<Candle>, KisError> {
        let raw = self.fetch_raw(symbol, start_date, end_date, "day").await?;
        parse_stock_candles(&raw)
    }

    /// 네이버 금융 API에서 지수 일봉 데이터 가져오기
    async fn fetch_index_daily(
        &self,
        symbol: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<IndexCandle>, KisError> {
        let raw = self.fetch_raw(symbol, start_date, end_date, "day").await?;
        parse_index_candles(&raw)
    }

    /// 네이버 금융 HTTP 요청
    async fn fetch_raw(
        &self,
        symbol: &str,
        start_date: &str,
        end_date: &str,
        timeframe: &str,
    ) -> Result<String, KisError> {
        let url = format!(
            "{NAVER_CHART_URL}?symbol={symbol}&requestType=1&startTime={start_date}&endTime={end_date}&timeframe={timeframe}"
        );

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| KisError::HttpError(format!("네이버 금융 요청 실패: {e}")))?;

        if !response.status().is_success() {
            return Err(KisError::HttpError(format!(
                "네이버 금융 HTTP {}: {symbol}",
                response.status()
            )));
        }

        response
            .text()
            .await
            .map_err(|e| KisError::HttpError(format!("응답 읽기 실패: {e}")))
    }
}

/// 네이버 금융 응답 파싱 (종목 일봉 → Candle)
fn parse_stock_candles(raw: &str) -> Result<Vec<Candle>, KisError> {
    let rows = parse_naver_rows(raw)?;
    let mut candles = Vec::new();

    for row in rows {
        if row.len() < 6 {
            continue;
        }

        let date_str = row[0].trim().trim_matches('"');
        let date = match NaiveDate::parse_from_str(date_str, "%Y%m%d") {
            Ok(d) => d,
            Err(_) => continue,
        };

        let open = parse_number(&row[1]);
        let high = parse_number(&row[2]);
        let low = parse_number(&row[3]);
        let close = parse_number(&row[4]);
        let volume = parse_number(&row[5]);

        // null 값(분봉) 필터링
        if open == 0 && high == 0 && low == 0 {
            continue;
        }

        candles.push(Candle {
            date,
            open,
            high,
            low,
            close,
            volume: volume as u64,
        });
    }

    Ok(candles)
}

/// 네이버 금융 응답 파싱 (지수 일봉 → IndexCandle)
fn parse_index_candles(raw: &str) -> Result<Vec<IndexCandle>, KisError> {
    let rows = parse_naver_rows(raw)?;
    let mut candles = Vec::new();

    for row in rows {
        if row.len() < 6 {
            continue;
        }

        let date_str = row[0].trim().trim_matches('"');
        let date = match NaiveDate::parse_from_str(date_str, "%Y%m%d") {
            Ok(d) => d,
            Err(_) => continue,
        };

        let open = parse_f64(&row[1]);
        let high = parse_f64(&row[2]);
        let low = parse_f64(&row[3]);
        let close = parse_f64(&row[4]);
        let volume = parse_number(&row[5]);

        if open == 0.0 && high == 0.0 {
            continue;
        }

        candles.push(IndexCandle {
            date,
            open,
            high,
            low,
            close,
            volume,
        });
    }

    Ok(candles)
}

/// 네이버 금융 유사-JSON 응답을 행 단위로 파싱
fn parse_naver_rows(raw: &str) -> Result<Vec<Vec<String>>, KisError> {
    // 작은따옴표 → 큰따옴표, 공백 정리
    let cleaned = raw.replace('\'', "\"");

    // JSON 배열로 파싱 시도
    let parsed: Vec<Vec<serde_json::Value>> = serde_json::from_str(&cleaned)
        .map_err(|e| KisError::ParseError(format!("네이버 금융 파싱 실패: {e}")))?;

    // 첫 행(헤더) 스킵, 나머지를 문자열 배열로 변환
    Ok(parsed
        .into_iter()
        .skip(1) // 헤더 행 스킵
        .map(|row| row.into_iter().map(|v| value_to_string(&v)).collect())
        .collect())
}

fn value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Null => "null".to_string(),
        _ => v.to_string(),
    }
}

fn parse_number(s: &str) -> i64 {
    let s = s.trim().trim_matches('"');
    if s == "null" || s.is_empty() {
        return 0;
    }
    s.parse::<f64>().unwrap_or(0.0) as i64
}

fn parse_f64(s: &str) -> f64 {
    let s = s.trim().trim_matches('"');
    if s == "null" || s.is_empty() {
        return 0.0;
    }
    s.parse::<f64>().unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_stock_candles() {
        let raw = r#"[['날짜', '시가', '고가', '저가', '종가', '거래량', '외국인소진율'],
["20260407", 56200, 57400, 55900, 57100, 18651234, 55.2],
["20260408", 57100, 57800, 56500, 57500, 15423567, 55.3]
]"#;
        let candles = parse_stock_candles(raw).unwrap();
        assert_eq!(candles.len(), 2);
        assert_eq!(candles[0].open, 56200);
        assert_eq!(candles[0].close, 57100);
        assert_eq!(candles[1].date, NaiveDate::from_ymd_opt(2026, 4, 8).unwrap());
    }

    #[test]
    fn test_parse_index_candles() {
        let raw = r#"[['날짜', '시가', '고가', '저가', '종가', '거래량', '외국인소진율'],
["20260407", 2650.12, 2680.45, 2640.88, 2670.33, 450000000, 0.0]
]"#;
        let candles = parse_index_candles(raw).unwrap();
        assert_eq!(candles.len(), 1);
        assert!((candles[0].close - 2670.33).abs() < 0.01);
    }

    #[test]
    fn test_parse_empty_response() {
        let raw = r#"[['날짜', '시가', '고가', '저가', '종가', '거래량', '외국인소진율']
]"#;
        let candles = parse_stock_candles(raw).unwrap();
        assert_eq!(candles.len(), 0);
    }
}
