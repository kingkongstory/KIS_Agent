use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Local, NaiveDateTime, NaiveTime, Timelike};
use tokio::sync::{RwLock, broadcast};
use tokio::time::Duration;
use tracing::{debug, info, warn};

use crate::domain::ports::realtime::{RealtimeCandleUpdate, RealtimeData, RealtimeExecution};
use crate::infrastructure::cache::postgres_store::{MinuteCandle, PostgresStore};

/// 집계 중인 부분 캔들
#[derive(Debug, Clone)]
struct PartialCandle {
    stock_code: String,
    /// 캔들 시작 시각 (분 슬롯: hour * 60 + minute)
    minute_slot: u32,
    /// 시각 문자열 "HH:MM"
    time_str: String,
    open: i64,
    high: i64,
    low: i64,
    close: i64,
    volume: u64,
    tick_count: u32,
}

impl PartialCandle {
    fn new(exec: &RealtimeExecution) -> Self {
        let (slot, time_str) = parse_tick_time(&exec.time);
        Self {
            stock_code: exec.stock_code.clone(),
            minute_slot: slot,
            time_str,
            open: exec.price,
            high: exec.price,
            low: exec.price,
            close: exec.price,
            volume: exec.volume,
            tick_count: 1,
        }
    }

    fn update(&mut self, exec: &RealtimeExecution) {
        self.high = self.high.max(exec.price);
        self.low = self.low.min(exec.price);
        self.close = exec.price;
        self.volume += exec.volume;
        self.tick_count += 1;
    }

    fn to_update(&self, is_closed: bool) -> RealtimeCandleUpdate {
        RealtimeCandleUpdate {
            stock_code: self.stock_code.clone(),
            time: self.time_str.clone(),
            open: self.open,
            high: self.high,
            low: self.low,
            close: self.close,
            volume: self.volume,
            is_closed,
        }
    }
}

/// 완성된 분봉 캔들
#[derive(Debug, Clone, serde::Serialize)]
pub struct CompletedCandle {
    pub stock_code: String,
    pub time: String,
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: u64,
    /// true = WebSocket 실시간 수집 (OHLCV 정확), false = 외부 백필 (종가 근사)
    #[serde(skip)]
    pub is_realtime: bool,
}

/// WebSocket 틱 → 1분봉 실시간 집계기
pub struct CandleAggregator {
    /// 완성된 캔들 히스토리 (종목별)
    completed: Arc<RwLock<HashMap<String, Vec<CompletedCandle>>>>,
    /// PostgreSQL 저장소 (실시간 분봉 영속화)
    store: Option<Arc<PostgresStore>>,
}

impl CandleAggregator {
    pub fn new() -> Self {
        Self {
            completed: Arc::new(RwLock::new(HashMap::new())),
            store: None,
        }
    }

    pub fn with_store(mut self, store: Arc<PostgresStore>) -> Self {
        self.store = Some(store);
        self
    }

    /// 특정 종목의 완성된 캔들 목록 조회
    pub fn completed_candles(&self) -> Arc<RwLock<HashMap<String, Vec<CompletedCandle>>>> {
        Arc::clone(&self.completed)
    }

    /// DB에서 당일 분봉을 로딩하여 completed에 주입 (장중 재시작 복구)
    pub async fn preload_from_db(&self, stock_codes: &[&str]) {
        let Some(ref store) = self.store else { return };

        let today = Local::now().date_naive();
        let start = today.and_hms_opt(9, 0, 0).unwrap();
        let end = today.and_hms_opt(15, 30, 0).unwrap();

        let mut total = 0usize;
        let mut completed = self.completed.write().await;

        for code in stock_codes {
            match store.get_minute_ohlcv(code, start, end, 1).await {
                Ok(candles) if !candles.is_empty() => {
                    let bars: Vec<CompletedCandle> = candles.iter().map(|c| {
                        // OHLC에 범위가 있으면 실시간 데이터 (FVG 탐색 허용)
                        let has_range = c.open != c.close || c.high != c.low;
                        CompletedCandle {
                            stock_code: code.to_string(),
                            time: format!("{:02}:{:02}", c.datetime.time().hour(), c.datetime.time().minute()),
                            open: c.open,
                            high: c.high,
                            low: c.low,
                            close: c.close,
                            volume: c.volume as u64,
                            is_realtime: has_range,
                        }
                    }).collect();
                    let count = bars.len();
                    completed.insert(code.to_string(), bars);
                    total += count;
                    info!("{}: DB에서 당일 분봉 {}건 로딩", code, count);
                }
                Ok(_) => {}
                Err(e) => warn!("{}: DB 분봉 로딩 실패: {e}", code),
            }
        }

        if total > 0 {
            info!("분봉 프리로딩 완료: 총 {}건", total);
        }
    }

    /// OR 데이터(09:00~09:15)가 없으면 네이버 금융에서 당일 분봉 자동 보충
    pub async fn backfill_from_naver(&self, stock_codes: &[&str]) {
        let or_start = NaiveTime::from_hms_opt(9, 0, 0).unwrap();

        // OR 데이터가 이미 있는지 확인
        {
            let completed = self.completed.read().await;
            let all_have_or = stock_codes.iter().all(|code| {
                completed.get(*code).map_or(false, |bars| {
                    bars.iter().any(|c| {
                        let parts: Vec<&str> = c.time.split(':').collect();
                        let h: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(99);
                        let m: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                        NaiveTime::from_hms_opt(h, m, 0).map_or(false, |t| t >= or_start && t < NaiveTime::from_hms_opt(9, 15, 0).unwrap())
                    })
                })
            });
            if all_have_or {
                info!("OR 데이터 이미 존재 — 네이버 백필 불필요");
                return;
            }
        }

        info!("OR 데이터 부족 — 네이버 금융에서 당일 분봉 보충 시작");

        let client = reqwest::Client::new();
        let today = Local::now().date_naive();

        for code in stock_codes {
            match fetch_naver_today_ticks(&client, code).await {
                Ok(ticks) if !ticks.is_empty() => {
                    let bars: Vec<CompletedCandle> = ticks.iter()
                        .filter(|t| t.datetime.date() == today)
                        .map(|t| CompletedCandle {
                            stock_code: code.to_string(),
                            time: format!("{:02}:{:02}", t.datetime.time().hour(), t.datetime.time().minute()),
                            open: t.close,
                            high: t.close,
                            low: t.close,
                            close: t.close,
                            volume: 0,
                            is_realtime: false,
                        })
                        .collect();

                    if bars.is_empty() {
                        warn!("{}: 네이버 당일 데이터 없음", code);
                        continue;
                    }

                    // DB에도 저장 (다음 재시작 시 활용)
                    if let Some(ref store) = self.store {
                        let db_candles: Vec<MinuteCandle> = bars.iter().map(|b| {
                            let parts: Vec<&str> = b.time.split(':').collect();
                            let h: u32 = parts[0].parse().unwrap_or(0);
                            let m: u32 = parts[1].parse().unwrap_or(0);
                            let time = NaiveTime::from_hms_opt(h, m, 0).unwrap_or_default();
                            MinuteCandle {
                                datetime: NaiveDateTime::new(today, time),
                                open: b.open, high: b.high, low: b.low, close: b.close,
                                volume: 0, interval_min: 1,
                            }
                        }).collect();
                        if let Err(e) = store.save_minute_ohlcv(code, &db_candles).await {
                            warn!("{}: 네이버 분봉 DB 저장 실패: {e}", code);
                        }
                    }

                    let count = bars.len();
                    // 기존 데이터와 병합 (중복 시각은 기존 우선)
                    let mut completed = self.completed.write().await;
                    let existing = completed.entry(code.to_string()).or_default();
                    for bar in bars {
                        if !existing.iter().any(|e| e.time == bar.time) {
                            existing.push(bar);
                        }
                    }
                    existing.sort_by(|a, b| a.time.cmp(&b.time));
                    info!("{}: 네이버에서 당일 {}봉 보충 완료", code, count);
                }
                Ok(_) => warn!("{}: 네이버 데이터 비어 있음", code),
                Err(e) => warn!("{}: 네이버 조회 실패: {e}", code),
            }
        }
    }

    /// 백그라운드 집계 태스크 시작
    pub fn spawn(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<RealtimeData>,
        tx: broadcast::Sender<RealtimeData>,
    ) {
        let completed = Arc::clone(&self.completed);
        let store = self.store.clone();

        tokio::spawn(async move {
            info!("분봉 집계기 시작");

            // 종목별 현재 집계 중인 캔들
            let mut partials: HashMap<String, PartialCandle> = HashMap::new();

            loop {
                // 60초 타임아웃: 장 마감 후 미완성 캔들 강제 완성용
                match tokio::time::timeout(Duration::from_secs(60), rx.recv()).await {
                    Ok(Ok(RealtimeData::Execution(exec))) => {
                        let (slot, _) = parse_tick_time(&exec.time);

                        // 장중 시간 필터 (09:00 ~ 15:30)
                        if slot < 540 || slot > 930 {
                            continue;
                        }

                        let code = exec.stock_code.clone();

                        if let Some(partial) = partials.get_mut(&code) {
                            if partial.minute_slot == slot {
                                // 같은 분 → 업데이트
                                partial.update(&exec);

                                // 10틱마다 진행 중 캔들 push (UI 실시간 갱신)
                                if partial.tick_count % 10 == 0 {
                                    let update = partial.to_update(false);
                                    let _ = tx.send(RealtimeData::CandleUpdate(update));
                                }
                            } else {
                                // 분 경계 넘김 → 이전 캔들 완성
                                let closed = partial.to_update(true);
                                debug!(
                                    "분봉 완성: {} {} O={} H={} L={} C={} V={} ({}틱)",
                                    closed.stock_code, closed.time,
                                    closed.open, closed.high, closed.low, closed.close,
                                    closed.volume, partial.tick_count
                                );

                                // 완성 캔들 저장
                                {
                                    let mut store = completed.write().await;
                                    store
                                        .entry(code.clone())
                                        .or_default()
                                        .push(CompletedCandle {
                                            stock_code: closed.stock_code.clone(),
                                            time: closed.time.clone(),
                                            open: closed.open,
                                            high: closed.high,
                                            low: closed.low,
                                            close: closed.close,
                                            volume: closed.volume,
                                            is_realtime: true,
                                        });
                                }

                                // DB 저장
                                if let Some(ref db) = store {
                                    save_candle_to_db(db, &closed).await;
                                }

                                // 완성 캔들 broadcast
                                let _ = tx.send(RealtimeData::CandleUpdate(closed));

                                // 새 캔들 시작
                                partials.insert(code, PartialCandle::new(&exec));
                            }
                        } else {
                            // 첫 틱 → 새 캔들 시작
                            info!("분봉 집계 시작: {} ({})", code, exec.time);
                            partials.insert(code, PartialCandle::new(&exec));
                        }
                    }
                    Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                        warn!("분봉 집계기: {n}건 메시지 누락 (버퍼 초과)");
                    }
                    Ok(Err(broadcast::error::RecvError::Closed)) => {
                        info!("분봉 집계기 종료 (채널 닫힘)");
                        break;
                    }
                    Ok(Ok(_)) => {} // Execution 외 메시지 무시
                    Err(_) => {
                        // 60초간 틱 미수신 → 미완성 캔들 강제 완성 (장 마감 등)
                        if !partials.is_empty() {
                            info!("60초 무활동 — 미완성 캔들 {}건 강제 완성", partials.len());
                            for (code, partial) in partials.drain() {
                                let closed = partial.to_update(true);
                                info!(
                                    "강제 완성: {} {} O={} H={} L={} C={} V={}",
                                    closed.stock_code, closed.time,
                                    closed.open, closed.high, closed.low, closed.close,
                                    closed.volume,
                                );
                                {
                                    let mut mem = completed.write().await;
                                    mem
                                        .entry(code)
                                        .or_default()
                                        .push(CompletedCandle {
                                            stock_code: closed.stock_code.clone(),
                                            time: closed.time.clone(),
                                            open: closed.open,
                                            high: closed.high,
                                            low: closed.low,
                                            close: closed.close,
                                            volume: closed.volume,
                                            is_realtime: true,
                                        });
                                }
                                if let Some(ref db) = store {
                                    save_candle_to_db(db, &closed).await;
                                }
                                let _ = tx.send(RealtimeData::CandleUpdate(closed));
                            }
                        }
                    }
                }
            }
        });
    }
}

/// 네이버 금융 1분 틱 (종가 기반)
#[derive(Debug)]
struct NaverTick {
    datetime: NaiveDateTime,
    close: i64,
}

/// 네이버 금융에서 당일 1분 틱 조회
async fn fetch_naver_today_ticks(client: &reqwest::Client, stock_code: &str) -> Result<Vec<NaverTick>, String> {
    let url = format!(
        "https://fchart.stock.naver.com/siseJson.naver?symbol={stock_code}&requestType=1&startTime=20260101&endTime=20261231&timeframe=minute"
    );

    let resp = client.get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .send().await
        .map_err(|e| format!("네이버 요청 실패: {e}"))?;

    let raw = resp.text().await.map_err(|e| format!("응답 읽기 실패: {e}"))?;
    let cleaned = raw.replace('\'', "\"");

    let parsed: Vec<Vec<serde_json::Value>> = serde_json::from_str(&cleaned)
        .map_err(|e| format!("파싱 실패: {e}"))?;

    let market_open = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
    let market_close = NaiveTime::from_hms_opt(15, 30, 0).unwrap();

    let ticks: Vec<NaverTick> = parsed.into_iter()
        .skip(1)
        .filter_map(|row| {
            if row.len() < 6 { return None; }
            let dt_str = row[0].as_str()?.trim().trim_matches('"');
            if dt_str.len() != 12 { return None; }
            let datetime = NaiveDateTime::parse_from_str(dt_str, "%Y%m%d%H%M").ok()?;
            let time = datetime.time();
            if time < market_open || time > market_close { return None; }
            let close = row[4].as_i64()?;
            if close <= 0 { return None; }
            Some(NaverTick { datetime, close })
        })
        .collect();

    Ok(ticks)
}

/// 완성 캔들을 PostgreSQL에 저장
async fn save_candle_to_db(store: &PostgresStore, candle: &RealtimeCandleUpdate) {
    let today = Local::now().date_naive();
    let parts: Vec<&str> = candle.time.split(':').collect();
    let (h, m) = (
        parts.first().and_then(|s| s.parse().ok()).unwrap_or(0u32),
        parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0u32),
    );
    let time = chrono::NaiveTime::from_hms_opt(h, m, 0).unwrap_or_default();
    let datetime = today.and_time(time);

    let mc = MinuteCandle {
        datetime,
        open: candle.open,
        high: candle.high,
        low: candle.low,
        close: candle.close,
        volume: candle.volume as i64,
        interval_min: 1,
    };
    if let Err(e) = store.save_minute_ohlcv(&candle.stock_code, &[mc]).await {
        warn!("분봉 DB 저장 실패: {e}");
    }
}

/// 틱 시각 문자열("HHMMSS") → (분 슬롯, "HH:MM")
fn parse_tick_time(time_str: &str) -> (u32, String) {
    if time_str.len() >= 4 {
        let hour: u32 = time_str[..2].parse().unwrap_or(0);
        let minute: u32 = time_str[2..4].parse().unwrap_or(0);
        let slot = hour * 60 + minute;
        let formatted = format!("{:02}:{:02}", hour, minute);
        (slot, formatted)
    } else {
        (0, "00:00".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tick_time() {
        assert_eq!(parse_tick_time("093015"), (9 * 60 + 30, "09:30".to_string()));
        assert_eq!(parse_tick_time("150000"), (15 * 60, "15:00".to_string()));
        assert_eq!(parse_tick_time("090000"), (540, "09:00".to_string()));
    }

    #[test]
    fn test_partial_candle_new_and_update() {
        let exec1 = RealtimeExecution {
            stock_code: "005930".to_string(),
            time: "093000".to_string(),
            price: 72000,
            change: 100,
            change_rate: 0.14,
            volume: 500,
            ask_price: 72100,
            bid_price: 71900,
            open: 71500,
            high: 72500,
            low: 71000,
        };

        let mut candle = PartialCandle::new(&exec1);
        assert_eq!(candle.open, 72000);
        assert_eq!(candle.high, 72000);
        assert_eq!(candle.low, 72000);
        assert_eq!(candle.volume, 500);

        let exec2 = RealtimeExecution {
            price: 72500,
            volume: 300,
            time: "093015".to_string(),
            ..exec1.clone()
        };
        candle.update(&exec2);
        assert_eq!(candle.high, 72500);
        assert_eq!(candle.close, 72500);
        assert_eq!(candle.volume, 800);

        let exec3 = RealtimeExecution {
            price: 71800,
            volume: 200,
            time: "093030".to_string(),
            ..exec1
        };
        candle.update(&exec3);
        assert_eq!(candle.low, 71800);
        assert_eq!(candle.close, 71800);
        assert_eq!(candle.volume, 1000);
        assert_eq!(candle.tick_count, 3);
    }

    #[test]
    fn test_to_update() {
        let exec = RealtimeExecution {
            stock_code: "122630".to_string(),
            time: "100500".to_string(),
            price: 15000,
            change: 50,
            change_rate: 0.33,
            volume: 1000,
            ask_price: 15010,
            bid_price: 14990,
            open: 14900,
            high: 15100,
            low: 14800,
        };

        let candle = PartialCandle::new(&exec);
        let update = candle.to_update(true);
        assert_eq!(update.stock_code, "122630");
        assert_eq!(update.time, "10:05");
        assert!(update.is_closed);
        assert_eq!(update.open, 15000);
    }

    #[tokio::test]
    async fn test_aggregator_minute_boundary() {
        let (tx, _rx) = broadcast::channel::<RealtimeData>(64);
        let aggregator = Arc::new(CandleAggregator::new());
        let completed = aggregator.completed_candles();

        // 집계기 시작
        let rx_for_agg = tx.subscribe();
        let tx_clone = tx.clone();
        Arc::clone(&aggregator).spawn(rx_for_agg, tx_clone);

        // 09:30 틱 2개 전송
        let exec1 = RealtimeExecution {
            stock_code: "005930".to_string(),
            time: "093000".to_string(),
            price: 72000,
            change: 0,
            change_rate: 0.0,
            volume: 100,
            ask_price: 0,
            bid_price: 0,
            open: 0,
            high: 0,
            low: 0,
        };
        let _ = tx.send(RealtimeData::Execution(exec1));

        let exec2 = RealtimeExecution {
            stock_code: "005930".to_string(),
            time: "093030".to_string(),
            price: 72500,
            change: 0,
            change_rate: 0.0,
            volume: 200,
            ask_price: 0,
            bid_price: 0,
            open: 0,
            high: 0,
            low: 0,
        };
        let _ = tx.send(RealtimeData::Execution(exec2));

        // 09:31 틱 → 09:30 캔들 완성 트리거
        let exec3 = RealtimeExecution {
            stock_code: "005930".to_string(),
            time: "093100".to_string(),
            price: 72300,
            change: 0,
            change_rate: 0.0,
            volume: 150,
            ask_price: 0,
            bid_price: 0,
            open: 0,
            high: 0,
            low: 0,
        };
        let _ = tx.send(RealtimeData::Execution(exec3));

        // 비동기 처리 대기
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // 완성된 캔들 검증
        let store = completed.read().await;
        let candles = store.get("005930").expect("캔들이 있어야 함");
        assert_eq!(candles.len(), 1);
        assert_eq!(candles[0].time, "09:30");
        assert_eq!(candles[0].open, 72000);
        assert_eq!(candles[0].high, 72500);
        assert_eq!(candles[0].low, 72000);
        assert_eq!(candles[0].close, 72500);
        assert_eq!(candles[0].volume, 300);
    }
}
