use std::sync::Arc;

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::infrastructure::cache::postgres_store::{MinuteCandle, PostgresStore};

const NAVER_CHART_URL: &str = "https://fchart.stock.naver.com/siseJson.naver";

/// 네이버 금융 분봉 수집기
///
/// 네이버 minute API는 종가 + 누적거래량만 제공 (OHLC = null).
/// 1분 종가를 5분봉 OHLCV로 집계하여 DB에 저장한다.
pub struct NaverMinuteCollector {
    client: reqwest::Client,
    store: Arc<PostgresStore>,
}

/// 네이버 1분 틱 데이터 (파싱 결과)
#[derive(Debug, Clone)]
struct MinuteTick {
    datetime: NaiveDateTime,
    close: i64,
    cumulative_volume: i64,
}

impl NaverMinuteCollector {
    pub fn new(store: Arc<PostgresStore>) -> Self {
        Self {
            client: reqwest::Client::new(),
            store,
        }
    }

    /// 종목의 분봉 수집 + DB 저장 (네이버에서 제공하는 전체 기간)
    pub async fn collect(
        &self,
        stock_code: &str,
        interval_min: i16,
    ) -> Result<usize, KisError> {
        // 1) 네이버에서 1분 틱 데이터 가져오기
        let ticks = self.fetch_minute_ticks(stock_code).await?;
        if ticks.is_empty() {
            warn!("네이버 분봉: {stock_code} 데이터 없음");
            return Ok(0);
        }

        // 날짜 범위 로깅
        let first_date = ticks.last().map(|t| t.datetime.date()).unwrap();
        let last_date = ticks.first().map(|t| t.datetime.date()).unwrap();
        info!(
            "네이버 분봉: {stock_code} 틱 {}건 ({first_date} ~ {last_date})",
            ticks.len()
        );

        // 2) 날짜별로 그룹핑 → N분봉 집계
        let mut all_candles = Vec::new();
        let dates = collect_unique_dates(&ticks);

        for date in &dates {
            let day_ticks: Vec<_> = ticks
                .iter()
                .filter(|t| t.datetime.date() == *date)
                .cloned()
                .collect();

            let candles = aggregate_ticks_to_candles(&day_ticks, interval_min);
            all_candles.extend(candles);
        }

        if all_candles.is_empty() {
            warn!("네이버 분봉: {stock_code} 집계 결과 없음");
            return Ok(0);
        }

        // 3) DB 저장
        let count = self.store.save_minute_ohlcv(stock_code, &all_candles).await?;
        info!(
            "네이버 분봉: {stock_code} {interval_min}분봉 {count}건 저장 ({} ~ {})",
            dates.first().unwrap(),
            dates.last().unwrap()
        );
        Ok(count)
    }

    /// 여러 종목 일괄 수집
    pub async fn collect_batch(
        &self,
        stock_codes: &[&str],
        interval_min: i16,
    ) -> Result<usize, KisError> {
        let mut total = 0;
        for code in stock_codes {
            match self.collect(code, interval_min).await {
                Ok(count) => total += count,
                Err(e) => warn!("네이버 분봉: {code} 수집 실패: {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        Ok(total)
    }

    /// 네이버 금융 1분 틱 데이터 가져오기
    async fn fetch_minute_ticks(&self, stock_code: &str) -> Result<Vec<MinuteTick>, KisError> {
        // 최대한 넓은 범위 요청 (네이버가 보유한 만큼만 반환)
        let url = format!(
            "{NAVER_CHART_URL}?symbol={stock_code}&requestType=1&startTime=20260101&endTime=20261231&timeframe=minute"
        );

        let response = self
            .client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0")
            .send()
            .await
            .map_err(|e| KisError::HttpError(format!("네이버 분봉 요청 실패: {e}")))?;

        let raw = response
            .text()
            .await
            .map_err(|e| KisError::HttpError(format!("응답 읽기 실패: {e}")))?;

        parse_minute_ticks(&raw)
    }
}

/// 네이버 응답 파싱 → MinuteTick 목록
fn parse_minute_ticks(raw: &str) -> Result<Vec<MinuteTick>, KisError> {
    let cleaned = raw.replace('\'', "\"");

    let parsed: Vec<Vec<serde_json::Value>> = serde_json::from_str(&cleaned)
        .map_err(|e| KisError::ParseError(format!("네이버 분봉 파싱 실패: {e}")))?;

    let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();

    let ticks: Vec<MinuteTick> = parsed
        .into_iter()
        .skip(1) // 헤더 스킵
        .filter_map(|row| {
            if row.len() < 6 {
                return None;
            }

            // 날짜 파싱 (YYYYMMDDHHMM, 12자리)
            let dt_str = row[0].as_str()?.trim().trim_matches('"');
            if dt_str.len() != 12 {
                return None;
            }
            let datetime = NaiveDateTime::parse_from_str(dt_str, "%Y%m%d%H%M").ok()?;
            let time = datetime.time();

            // 장 시간 필터 (09:00 ~ 15:30)
            if time < market_open || time > market_close {
                return None;
            }

            // 종가 (4번째 컬럼, null이면 스킵)
            let close = match &row[4] {
                serde_json::Value::Number(n) => n.as_i64()?,
                _ => return None,
            };

            // 누적 거래량 (5번째 컬럼)
            let cum_vol = match &row[5] {
                serde_json::Value::Number(n) => n.as_i64().unwrap_or(0),
                _ => 0,
            };

            if close <= 0 {
                return None;
            }

            Some(MinuteTick {
                datetime,
                close,
                cumulative_volume: cum_vol,
            })
        })
        .collect();

    Ok(ticks)
}

/// 고유 날짜 목록 추출 (오름차순)
fn collect_unique_dates(ticks: &[MinuteTick]) -> Vec<NaiveDate> {
    let mut dates: Vec<NaiveDate> = ticks.iter().map(|t| t.datetime.date()).collect();
    dates.sort();
    dates.dedup();
    dates
}

/// 1분 틱 → N분봉 OHLCV 집계
///
/// 종가만 있으므로 근사 캔들 생성:
/// - Open = 슬롯 내 첫 번째 종가
/// - High = 슬롯 내 최대 종가
/// - Low = 슬롯 내 최소 종가
/// - Close = 슬롯 내 마지막 종가
/// - Volume = 누적거래량 차이 (마지막 - 첫 번째)
fn aggregate_ticks_to_candles(day_ticks: &[MinuteTick], interval_min: i16) -> Vec<MinuteCandle> {
    if day_ticks.is_empty() {
        return Vec::new();
    }

    // 시간 오름차순 정렬
    let mut sorted = day_ticks.to_vec();
    sorted.sort_by_key(|t| t.datetime);

    let market_open_min = 9 * 60; // 09:00 = 540분

    // 슬롯 번호 계산
    let slot_key = |dt: &NaiveDateTime| -> u32 {
        let total_min = dt.time().hour() * 60 + dt.time().minute();
        let from_open = total_min.saturating_sub(market_open_min);
        from_open / interval_min as u32
    };

    let mut candles = Vec::new();
    let mut i = 0;

    while i < sorted.len() {
        let key = slot_key(&sorted[i].datetime);
        let mut j = i + 1;
        while j < sorted.len() && slot_key(&sorted[j].datetime) == key {
            j += 1;
        }

        let slot = &sorted[i..j];
        let first = &slot[0];
        let last = &slot[slot.len() - 1];

        let open = first.close;
        let close = last.close;
        let high = slot.iter().map(|t| t.close).max().unwrap();
        let low = slot.iter().map(|t| t.close).min().unwrap();
        let volume = (last.cumulative_volume - first.cumulative_volume).max(0);

        // 캔들 시작 시각 = 슬롯의 첫 번째 시각
        let slot_start_min = market_open_min + key * interval_min as u32;
        let candle_time = NaiveTime::from_hms_opt(slot_start_min / 60, slot_start_min % 60, 0)
            .unwrap_or(first.datetime.time());
        let candle_dt = NaiveDateTime::new(first.datetime.date(), candle_time);

        candles.push(MinuteCandle {
            datetime: candle_dt,
            open,
            high,
            low,
            close,
            volume,
            interval_min,
        });

        i = j;
    }

    candles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minute_ticks() {
        let raw = r#"[['날짜', '시가', '고가', '저가', '종가', '거래량', '외국인소진율'],
["202604080900", null, null, null, 90000, 1000000, null],
["202604080901", null, null, null, 90500, 1050000, null],
["202604080902", null, null, null, 89800, 1120000, null]
]"#;
        let ticks = parse_minute_ticks(raw).unwrap();
        assert_eq!(ticks.len(), 3);
        assert_eq!(ticks[0].close, 90000);
        assert_eq!(ticks[2].close, 89800);
    }

    #[test]
    fn test_aggregate_ticks_5m() {
        let date = NaiveDate::from_ymd_opt(2026, 4, 8).unwrap();
        let ticks: Vec<MinuteTick> = (0..10)
            .map(|m| {
                let prices = [100, 105, 98, 103, 107, 110, 108, 112, 106, 115];
                MinuteTick {
                    datetime: NaiveDateTime::new(
                        date,
                        NaiveTime::from_hms_opt(9, m as u32, 0).unwrap(),
                    ),
                    close: prices[m as usize] * 100,
                    cumulative_volume: (m + 1) * 10000,
                }
            })
            .collect();

        let candles = aggregate_ticks_to_candles(&ticks, 5);
        assert_eq!(candles.len(), 2);

        // 첫 5분봉 (09:00~09:04): open=100, high=107, low=98, close=107
        assert_eq!(candles[0].open, 10000);
        assert_eq!(candles[0].high, 10700);
        assert_eq!(candles[0].low, 9800);
        assert_eq!(candles[0].close, 10700);

        // 두번째 5분봉 (09:05~09:09): open=110, high=115, low=106, close=115
        assert_eq!(candles[1].open, 11000);
        assert_eq!(candles[1].high, 11500);
        assert_eq!(candles[1].low, 10600);
        assert_eq!(candles[1].close, 11500);
    }

    #[test]
    fn test_filter_market_hours() {
        let raw = r#"[['날짜', '시가', '고가', '저가', '종가', '거래량', '외국인소진율'],
["202604080830", null, null, null, 80000, 10, null],
["202604080900", null, null, null, 90000, 1000, null],
["202604081530", null, null, null, 91000, 25000, null],
["202604081555", null, null, null, 91000, 26000, null]
]"#;
        let ticks = parse_minute_ticks(raw).unwrap();
        // 08:30 → 제외, 15:55 → 제외
        assert_eq!(ticks.len(), 2);
    }
}
