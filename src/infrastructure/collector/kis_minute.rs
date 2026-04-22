use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{info, warn};

use crate::domain::error::KisError;
use crate::domain::serde_utils::string_to_i64;
use crate::domain::types::TransactionId;
use crate::infrastructure::cache::postgres_store::{MinuteCandle, PostgresStore};
use crate::infrastructure::kis_client::http_client::{HttpMethod, KisHttpClient, KisResponse};

/// KIS API 당일 분봉 수집기
pub struct KisMinuteCollector {
    client: Arc<KisHttpClient>,
    store: Arc<PostgresStore>,
}

/// KIS 분봉 응답 항목 — `acml_vol` 은 JSON 에 포함되어야 deserialize 가 누락 없이
/// 성공하므로 필드로 유지하되 내부 로직엔 쓰이지 않는다.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct MinutePriceItem {
    /// 영업일자 (YYYYMMDD)
    stck_bsop_date: String,
    /// 체결시간 (HHMMSS)
    stck_cntg_hour: String,
    /// 시가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_oprc: i64,
    /// 고가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_hgpr: i64,
    /// 저가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_lwpr: i64,
    /// 현재가 (종가)
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    stck_prpr: i64,
    /// 거래량
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    cntg_vol: i64,
    /// 누적거래량
    #[serde(default, deserialize_with = "string_to_i64::deserialize")]
    acml_vol: i64,
}

impl KisMinuteCollector {
    pub fn new(client: Arc<KisHttpClient>, store: Arc<PostgresStore>) -> Self {
        Self { client, store }
    }

    /// 당일 1분봉 수집 및 PostgreSQL 저장
    pub async fn collect_today(&self, stock_code: &str) -> Result<usize, KisError> {
        let today = chrono::Local::now().format("%Y%m%d").to_string();
        self.collect_minute(stock_code, &today, 1).await
    }

    /// 특정일 분봉 수집 (interval: 1분, 5분, 10분 등)
    pub async fn collect_minute(
        &self,
        stock_code: &str,
        date: &str,
        interval: i16,
    ) -> Result<usize, KisError> {
        let mut all_items = Vec::new();
        let mut fid_input_hour = "153000".to_string(); // 장 마감부터 역순 조회
        // 09:00~15:30 = 390분, 페이지당 30개 → 13페이지 + 여유 1
        let max_pages = 14;

        for page in 0..max_pages {
            let query = [
                ("FID_ETC_CLS_CODE", ""),
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", stock_code),
                ("FID_INPUT_HOUR_1", &fid_input_hour),
                ("FID_PW_DATA_INCU_YN", "N"),
            ];

            let resp: KisResponse<Vec<MinutePriceItem>> = self
                .client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-time-itemchartprice",
                    &TransactionId::InquireTimePrice,
                    Some(&query),
                    None,
                )
                .await?;

            if resp.rt_cd != "0" {
                if page == 0 {
                    return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
                }
                break;
            }

            let has_next = resp.has_next();
            let items = resp.output.unwrap_or_default();
            if items.is_empty() {
                break;
            }

            // 마지막 항목의 시간을 다음 페이지 종료 시간으로
            if let Some(last) = items.last() {
                fid_input_hour = last.stck_cntg_hour.clone();
            }

            all_items.extend(items);

            if !has_next {
                break;
            }

            // 속도 제한 준수
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if all_items.is_empty() {
            warn!("KIS 분봉: {stock_code} 데이터 없음 (날짜: {date})");
            return Ok(0);
        }

        // MinutePriceItem → MinuteCandle 변환
        let candles: Vec<MinuteCandle> = all_items
            .iter()
            .filter_map(|item| {
                let date = NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let time = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                let datetime = NaiveDateTime::new(date, time);

                // 장 시간 필터링 (09:00 ~ 15:30)
                let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
                let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
                if time < market_open || time > market_close {
                    return None;
                }

                Some(MinuteCandle {
                    datetime,
                    open: item.stck_oprc,
                    high: item.stck_hgpr,
                    low: item.stck_lwpr,
                    close: item.stck_prpr,
                    volume: item.cntg_vol,
                    interval_min: interval,
                })
            })
            .collect();

        let count = self.store.save_minute_ohlcv(stock_code, &candles).await?;
        info!("KIS 분봉: {stock_code} {interval}분봉 {count}건 저장 완료 (날짜: {date})");
        Ok(count)
    }

    /// 과거 분봉 수집 (최대 1년, 주식일별분봉조회 API 사용).
    /// KIS 는 1분봉만 반환하므로 interval>1 이면 Rust 에서 OHLC 집계.
    /// 호출당 최대 120건 → 390분/120 ≈ 4페이지 역순.
    pub async fn collect_historical_minute(
        &self,
        stock_code: &str,
        date: &str, // YYYYMMDD
        interval: i16,
    ) -> Result<usize, KisError> {
        let target_date = NaiveDate::parse_from_str(date, "%Y%m%d")
            .map_err(|e| KisError::Internal(format!("날짜 형식 오류 (YYYYMMDD): {e}")))?;

        let mut all_items: Vec<MinutePriceItem> = Vec::new();
        let mut fid_input_hour = "153000".to_string();
        let max_pages = 6; // 390분/120 = 4 + 여유
        let mut seen_hour: std::collections::HashSet<String> = Default::default();

        for _page in 0..max_pages {
            let query = [
                ("FID_COND_MRKT_DIV_CODE", "J"),
                ("FID_INPUT_ISCD", stock_code),
                ("FID_INPUT_HOUR_1", fid_input_hour.as_str()),
                ("FID_INPUT_DATE_1", date),
                ("FID_PW_DATA_INCU_YN", "N"),
                ("FID_FAKE_TICK_INCU_YN", ""),
            ];

            let resp: KisResponse<Vec<MinutePriceItem>> = self
                .client
                .execute(
                    HttpMethod::Get,
                    "/uapi/domestic-stock/v1/quotations/inquire-time-dailychartprice",
                    &TransactionId::InquireDailyTimePrice,
                    Some(&query),
                    None,
                )
                .await?;

            if resp.rt_cd != "0" {
                return Err(KisError::classify(resp.rt_cd, resp.msg_cd, resp.msg1));
            }

            let items = resp.output2.unwrap_or_default();
            if items.is_empty() {
                break;
            }

            // target_date 이외 날짜는 drop
            let before = items.len();
            let mut filtered: Vec<MinutePriceItem> = items
                .into_iter()
                .filter(|it| it.stck_bsop_date == date)
                .collect();
            let filtered_len = filtered.len();

            let last_hour = filtered.last().map(|x| x.stck_cntg_hour.clone());
            all_items.append(&mut filtered);

            // 이전 영업일로 넘어가기 시작했으면 종료
            if filtered_len < before {
                break;
            }

            let Some(last_hour) = last_hour else {
                break;
            };

            // 같은 hour 가 반복되면 무한루프 방지
            if !seen_hour.insert(last_hour.clone()) {
                break;
            }

            // 09:00 이전이면 종료 (장 시작)
            if last_hour.as_str() <= "090000" {
                break;
            }

            fid_input_hour = last_hour;

            // KIS rate-limit 보호
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if all_items.is_empty() {
            warn!("KIS 일별분봉: {stock_code} {date} 데이터 없음");
            return Ok(0);
        }

        // 1분봉 MinuteCandle 로 변환 + 장시간 필터 + 시간 오름차순 정렬
        let mut one_min: Vec<MinuteCandle> = all_items
            .iter()
            .filter_map(|item| {
                let d = NaiveDate::parse_from_str(&item.stck_bsop_date, "%Y%m%d").ok()?;
                let t = NaiveTime::parse_from_str(&item.stck_cntg_hour, "%H%M%S").ok()?;
                let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
                let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();
                if t < market_open || t > market_close {
                    return None;
                }
                Some(MinuteCandle {
                    datetime: NaiveDateTime::new(d, t),
                    open: item.stck_oprc,
                    high: item.stck_hgpr,
                    low: item.stck_lwpr,
                    close: item.stck_prpr,
                    volume: item.cntg_vol,
                    interval_min: 1,
                })
            })
            .collect();
        one_min.sort_by_key(|c| c.datetime);
        // 같은 (datetime) 중복 제거 (마지막 것 유지)
        one_min.dedup_by_key(|c| c.datetime);

        // interval 집계
        let aggregated = aggregate_ohlc(&one_min, target_date, interval);

        let count = self
            .store
            .save_minute_ohlcv(stock_code, &aggregated)
            .await?;
        info!(
            "KIS 일별분봉: {stock_code} {date} {interval}분봉 {count}건 저장 (원본 1분봉 {})",
            one_min.len()
        );
        Ok(count)
    }

    /// 여러 종목 과거 분봉 일괄 수집
    pub async fn collect_historical_batch(
        &self,
        stock_codes: &[&str],
        date: &str,
        interval: i16,
    ) -> Result<usize, KisError> {
        let mut total = 0;
        for code in stock_codes {
            match self.collect_historical_minute(code, date, interval).await {
                Ok(count) => total += count,
                Err(e) => warn!("KIS 일별분봉: {code} {date} 수집 실패: {e}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        info!(
            "KIS 일별분봉: {}종목 {date} {interval}m 수집 완료, 총 {total}건",
            stock_codes.len()
        );
        Ok(total)
    }

    /// 여러 종목 당일 분봉 일괄 수집
    pub async fn collect_today_batch(&self, stock_codes: &[&str]) -> Result<usize, KisError> {
        let mut total = 0;
        for code in stock_codes {
            match self.collect_today(code).await {
                Ok(count) => total += count,
                Err(e) => warn!("KIS 분봉: {code} 수집 실패: {e}"),
            }
            // 속도 제한 (초당 제한 고려)
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        info!(
            "KIS 분봉: {}종목 당일 분봉 수집 완료, 총 {total}건",
            stock_codes.len()
        );
        Ok(total)
    }
}

/// 1분봉을 N분봉으로 집계.
/// 버킷 시작 시각 기준으로 그룹화 (예: interval=5 → 09:00, 09:05, ...).
/// open = 첫 분봉 open, high = max high, low = min low, close = 마지막 분봉 close, volume = sum.
fn aggregate_ohlc(
    one_min: &[MinuteCandle],
    _target_date: NaiveDate,
    interval: i16,
) -> Vec<MinuteCandle> {
    if interval <= 1 {
        return one_min.to_vec();
    }
    let step = interval as u32;
    use std::collections::BTreeMap;
    let mut buckets: BTreeMap<NaiveDateTime, Vec<&MinuteCandle>> = BTreeMap::new();
    for c in one_min {
        let t = c.datetime.time();
        let bucket_min = (t.hour() * 60 + t.minute()) / step * step;
        let (bh, bm) = (bucket_min / 60, bucket_min % 60);
        let bucket_time = NaiveTime::from_hms_opt(bh, bm, 0).unwrap();
        let bucket_dt = NaiveDateTime::new(c.datetime.date(), bucket_time);
        buckets.entry(bucket_dt).or_default().push(c);
    }
    buckets
        .into_iter()
        .map(|(dt, mut group)| {
            group.sort_by_key(|g| g.datetime);
            let open = group.first().unwrap().open;
            let close = group.last().unwrap().close;
            let high = group.iter().map(|g| g.high).max().unwrap();
            let low = group.iter().map(|g| g.low).min().unwrap();
            let volume: i64 = group.iter().map(|g| g.volume).sum();
            MinuteCandle {
                datetime: dt,
                open,
                high,
                low,
                close,
                volume,
                interval_min: interval,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(h: u32, m: u32, o: i64, hi: i64, lo: i64, cl: i64, v: i64) -> MinuteCandle {
        MinuteCandle {
            datetime: NaiveDateTime::new(
                NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
                NaiveTime::from_hms_opt(h, m, 0).unwrap(),
            ),
            open: o,
            high: hi,
            low: lo,
            close: cl,
            volume: v,
            interval_min: 1,
        }
    }

    #[test]
    fn aggregate_5m_from_1m() {
        let day = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let input = vec![
            c(9, 0, 100, 105, 99, 103, 10),
            c(9, 1, 103, 108, 102, 107, 20),
            c(9, 2, 107, 110, 106, 109, 30),
            c(9, 3, 109, 112, 108, 111, 40),
            c(9, 4, 111, 113, 110, 112, 50),
            c(9, 5, 112, 115, 111, 114, 60),
        ];
        let out = aggregate_ohlc(&input, day, 5);
        assert_eq!(out.len(), 2);
        // 첫 버킷 09:00: open=100, high=113, low=99, close=112, volume=150
        assert_eq!(out[0].datetime.time(), NaiveTime::from_hms_opt(9, 0, 0).unwrap());
        assert_eq!(out[0].open, 100);
        assert_eq!(out[0].high, 113);
        assert_eq!(out[0].low, 99);
        assert_eq!(out[0].close, 112);
        assert_eq!(out[0].volume, 150);
        assert_eq!(out[0].interval_min, 5);
        // 두 번째 버킷 09:05: 1건
        assert_eq!(out[1].datetime.time(), NaiveTime::from_hms_opt(9, 5, 0).unwrap());
        assert_eq!(out[1].close, 114);
    }

    #[test]
    fn aggregate_pass_through_1m() {
        let day = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let input = vec![c(9, 0, 100, 105, 99, 103, 10)];
        let out = aggregate_ohlc(&input, day, 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].interval_min, 1);
    }
}
