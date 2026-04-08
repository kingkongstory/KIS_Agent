use std::sync::Arc;

use chrono::{DateTime, NaiveDateTime, Utc};
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::infrastructure::cache::postgres_store::{MinuteCandle, PostgresStore};

const YAHOO_CHART_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";

/// Yahoo Finance 분봉 수집기
///
/// 한국 종목코드를 `.KS`(코스피) 또는 `.KQ`(코스닥) 접미사로 변환하여 조회.
/// 5분봉/15분봉 OHLCV 전체를 최대 1달치 제공.
pub struct YahooMinuteCollector {
    client: reqwest::Client,
    store: Arc<PostgresStore>,
}

impl YahooMinuteCollector {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self {
            client: reqwest::Client::new(),
            store,
        }
    }

    /// 종목의 분봉 수집 + DB 저장
    ///
    /// - `stock_code`: 한국 종목코드 (예: "069500")
    /// - `interval`: "5m", "15m" 등
    /// - `range`: "1mo", "5d" 등
    /// - `db_interval_min`: DB 저장 시 interval_min 값 (5, 15 등)
    pub async fn collect(
        &self,
        stock_code: &str,
        interval: &str,
        range: &str,
        db_interval_min: i16,
    ) -> Result<usize, KisError> {
        let yahoo_symbol = format!("{stock_code}.KS");

        let candles = self
            .fetch_candles(&yahoo_symbol, interval, range, db_interval_min)
            .await?;

        if candles.is_empty() {
            // 코스닥 종목일 수 있으므로 .KQ로 재시도
            let yahoo_symbol_kq = format!("{stock_code}.KQ");
            let candles_kq = self
                .fetch_candles(&yahoo_symbol_kq, interval, range, db_interval_min)
                .await?;
            if candles_kq.is_empty() {
                warn!("Yahoo 분봉: {stock_code} 데이터 없음");
                return Ok(0);
            }
            let count = self.store.save_minute_ohlcv(stock_code, &candles_kq).await?;
            info!("Yahoo 분봉: {stock_code} {interval} {count}건 저장");
            return Ok(count);
        }

        let count = self.store.save_minute_ohlcv(stock_code, &candles).await?;
        info!("Yahoo 분봉: {stock_code} {interval} {count}건 저장");
        Ok(count)
    }

    /// 여러 종목 일괄 수집
    pub async fn collect_batch(
        &self,
        stock_codes: &[&str],
        interval: &str,
        range: &str,
        db_interval_min: i16,
    ) -> Result<usize, KisError> {
        let mut total = 0;
        for code in stock_codes {
            match self.collect(code, interval, range, db_interval_min).await {
                Ok(count) => total += count,
                Err(e) => warn!("Yahoo 분봉: {code} 수집 실패: {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(total)
    }

    /// Yahoo Finance API에서 분봉 데이터 가져오기
    async fn fetch_candles(
        &self,
        yahoo_symbol: &str,
        interval: &str,
        range: &str,
        db_interval_min: i16,
    ) -> Result<Vec<MinuteCandle>, KisError> {
        let url = format!(
            "{YAHOO_CHART_URL}/{yahoo_symbol}?interval={interval}&range={range}"
        );

        let response = self
            .client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| KisError::HttpError(format!("Yahoo 요청 실패: {e}")))?;

        let text = response
            .text()
            .await
            .map_err(|e| KisError::HttpError(format!("Yahoo 응답 읽기 실패: {e}")))?;

        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| KisError::ParseError(format!("Yahoo JSON 파싱 실패: {e}")))?;

        let result = parsed["chart"]["result"]
            .as_array()
            .and_then(|a| a.first())
            .ok_or_else(|| KisError::ParseError("Yahoo 응답에 result 없음".into()))?;

        let timestamps = result["timestamp"]
            .as_array()
            .ok_or_else(|| KisError::ParseError("Yahoo 응답에 timestamp 없음".into()))?;

        let quote = &result["indicators"]["quote"][0];
        let opens = quote["open"].as_array();
        let highs = quote["high"].as_array();
        let lows = quote["low"].as_array();
        let closes = quote["close"].as_array();
        let volumes = quote["volume"].as_array();

        let (opens, highs, lows, closes, volumes) =
            match (opens, highs, lows, closes, volumes) {
                (Some(o), Some(h), Some(l), Some(c), Some(v)) => (o, h, l, c, v),
                _ => return Ok(Vec::new()),
            };

        let market_open_hour = 9;
        let market_close_hour = 15;
        let market_close_min = 30;

        let mut candles = Vec::new();
        for i in 0..timestamps.len() {
            let ts = timestamps[i].as_i64().unwrap_or(0);
            if ts == 0 {
                continue;
            }

            let dt = DateTime::<Utc>::from_timestamp(ts, 0)
                .map(|d| d.with_timezone(&chrono::FixedOffset::east_opt(9 * 3600).unwrap()))
                .map(|d| d.naive_local());

            let dt = match dt {
                Some(d) => d,
                None => continue,
            };

            let hour = dt.time().hour();
            let min = dt.time().minute();

            // 장 시간 필터 (09:00 ~ 15:30)
            if hour < market_open_hour
                || (hour == market_close_hour && min > market_close_min as u32)
                || hour > market_close_hour
            {
                continue;
            }

            let open = opens[i].as_f64().unwrap_or(0.0) as i64;
            let high = highs[i].as_f64().unwrap_or(0.0) as i64;
            let low = lows[i].as_f64().unwrap_or(0.0) as i64;
            let close = closes[i].as_f64().unwrap_or(0.0) as i64;
            let volume = volumes[i].as_i64().unwrap_or(0);

            if open == 0 || high == 0 || low == 0 || close == 0 {
                continue;
            }

            candles.push(MinuteCandle {
                datetime: dt,
                open,
                high,
                low,
                close,
                volume,
                interval_min: db_interval_min,
            });
        }

        info!(
            "Yahoo 분봉: {} {}건 조회 ({})",
            yahoo_symbol,
            candles.len(),
            range
        );
        Ok(candles)
    }
}

use chrono::Timelike;
