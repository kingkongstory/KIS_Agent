use serde::Deserialize;

use crate::domain::ports::realtime::{
    ExecutionNotice, MarketOperation, RealtimeData, RealtimeExecution, RealtimeOrderBook,
};

// KIS WebSocket 필드 인덱스는 공식 GitHub 샘플
// `legacy/Sample01/kis_domstk_ws.py` 의 `contract_cols` / `bid_ask_cols`
// 0-based 순서를 기준으로 고정한다.
// 2026-04-23에 로컬 문서 오기가 확인되어, parser 의 진실 원천은 공식 샘플과
// 회귀 테스트다. 스프레드 gate 와 분봉 집계가 이 값을 직접 쓰므로 magic number를
// 상수화해 인덱스 회귀를 즉시 드러내게 한다.
const EXEC_STOCK_CODE_IDX: usize = 0;
const EXEC_TIME_IDX: usize = 1;
const EXEC_PRICE_IDX: usize = 2;
const EXEC_CHANGE_IDX: usize = 4;
const EXEC_CHANGE_RATE_IDX: usize = 5;
const EXEC_OPEN_IDX: usize = 7;
const EXEC_HIGH_IDX: usize = 8;
const EXEC_LOW_IDX: usize = 9;
const EXEC_ASK_PRICE_IDX: usize = 10;
const EXEC_BID_PRICE_IDX: usize = 11;
const EXEC_TRADE_VOLUME_IDX: usize = 12;
const EXEC_MIN_FIELDS: usize = EXEC_TRADE_VOLUME_IDX + 1;

const ORDERBOOK_STOCK_CODE_IDX: usize = 0;
const ORDERBOOK_ASK_START_IDX: usize = 3;
const ORDERBOOK_BID_START_IDX: usize = 13;
const ORDERBOOK_ASK_QTY_START_IDX: usize = 23;
const ORDERBOOK_BID_QTY_START_IDX: usize = 33;
const ORDERBOOK_TOTAL_ASK_IDX: usize = 43;
const ORDERBOOK_TOTAL_BID_IDX: usize = 44;
const ORDERBOOK_LEVEL_COUNT: usize = 10;
const ORDERBOOK_MIN_FIELDS: usize = ORDERBOOK_TOTAL_BID_IDX + 1;

/// WebSocket 메시지 종류 (JSON 제어 vs 파이프 데이터)
#[derive(Debug)]
pub enum WsMessage {
    Control(ControlMessage),
    Data(DataMessage),
}

/// JSON 제어 메시지
#[derive(Debug, Deserialize)]
pub struct ControlMessage {
    pub header: ControlHeader,
    pub body: ControlBody,
}

#[derive(Debug, Deserialize)]
pub struct ControlHeader {
    pub tr_id: String,
    pub tr_key: String,
    #[serde(default)]
    pub encrypt: String,
}

#[derive(Debug, Deserialize)]
pub struct ControlBody {
    pub rt_cd: String,
    pub msg_cd: String,
    pub msg1: String,
    #[serde(default)]
    pub output: Option<serde_json::Value>,
}

/// 파이프 구분 데이터 메시지
#[derive(Debug)]
pub struct DataMessage {
    pub encrypted: bool,
    pub tr_id: String,
    pub count: usize,
    pub data_parts: Vec<String>,
}

/// WebSocket 메시지 파싱
pub fn parse_ws_message(text: &str) -> Option<WsMessage> {
    // JSON인지 파이프 구분인지 판별
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        // JSON 제어 메시지
        let msg: ControlMessage = serde_json::from_str(trimmed).ok()?;
        Some(WsMessage::Control(msg))
    } else {
        // 파이프 구분 데이터: "encrypted|tr_id|count|data"
        let parts: Vec<&str> = trimmed.splitn(4, '|').collect();
        if parts.len() < 4 {
            return None;
        }

        let encrypted = parts[0] == "1";
        let tr_id = parts[1].to_string();
        let count = parts[2].parse::<usize>().unwrap_or(1);
        let data_parts: Vec<String> = parts[3].split('^').map(|s| s.to_string()).collect();

        Some(WsMessage::Data(DataMessage {
            encrypted,
            tr_id,
            count,
            data_parts,
        }))
    }
}

/// 실시간 체결가 (H0STCNT0) 파싱
pub fn parse_execution(data: &DataMessage) -> Option<RealtimeData> {
    if data.data_parts.len() < EXEC_MIN_FIELDS {
        return None;
    }

    let parts = &data.data_parts;
    Some(RealtimeData::Execution(RealtimeExecution {
        stock_code: parts[EXEC_STOCK_CODE_IDX].clone(),
        time: parts[EXEC_TIME_IDX].clone(),
        price: parts[EXEC_PRICE_IDX].parse().unwrap_or(0),
        change: parts[EXEC_CHANGE_IDX].parse::<i64>().unwrap_or(0),
        change_rate: parts[EXEC_CHANGE_RATE_IDX].parse().unwrap_or(0.0),
        // 누적거래량(13)이 아니라 직전 체결량(12)을 분봉 집계에 사용해야 중복 누적이 없다.
        volume: parts[EXEC_TRADE_VOLUME_IDX].parse::<u64>().unwrap_or(0),
        ask_price: parts[EXEC_ASK_PRICE_IDX].parse().unwrap_or(0),
        bid_price: parts[EXEC_BID_PRICE_IDX].parse().unwrap_or(0),
        open: parts[EXEC_OPEN_IDX].parse().unwrap_or(0),
        high: parts[EXEC_HIGH_IDX].parse().unwrap_or(0),
        low: parts[EXEC_LOW_IDX].parse().unwrap_or(0),
    }))
}

/// 실시간 호가 (H0STASP0) 파싱
pub fn parse_orderbook(data: &DataMessage) -> Option<RealtimeData> {
    if data.data_parts.len() < ORDERBOOK_MIN_FIELDS {
        return None;
    }

    let parts = &data.data_parts;
    let stock_code = parts[ORDERBOOK_STOCK_CODE_IDX].clone();

    let mut asks = Vec::with_capacity(ORDERBOOK_LEVEL_COUNT);
    let mut bids = Vec::with_capacity(ORDERBOOK_LEVEL_COUNT);

    // 공식 문서 기준:
    // - 매도호가 1~10: 3..12
    // - 매수호가 1~10: 13..22
    // - 매도잔량 1~10: 23..32
    // - 매수잔량 1~10: 33..42
    for i in 0..ORDERBOOK_LEVEL_COUNT {
        let ask_price: i64 = parts[ORDERBOOK_ASK_START_IDX + i].parse().unwrap_or(0);
        let bid_price: i64 = parts[ORDERBOOK_BID_START_IDX + i].parse().unwrap_or(0);
        let ask_qty: i64 = parts[ORDERBOOK_ASK_QTY_START_IDX + i].parse().unwrap_or(0);
        let bid_qty: i64 = parts[ORDERBOOK_BID_QTY_START_IDX + i].parse().unwrap_or(0);
        asks.push((ask_price, ask_qty));
        bids.push((bid_price, bid_qty));
    }

    let total_ask: i64 = parts[ORDERBOOK_TOTAL_ASK_IDX].parse().unwrap_or(0);
    let total_bid: i64 = parts[ORDERBOOK_TOTAL_BID_IDX].parse().unwrap_or(0);

    Some(RealtimeData::OrderBook(RealtimeOrderBook {
        stock_code,
        asks,
        bids,
        total_ask_volume: total_ask,
        total_bid_volume: total_bid,
    }))
}

/// 체결 통보 (H0STCNI0/H0STCNI9) 파싱 — 복호화된 데이터 기준
/// 필드: 고객ID[0]|계좌번호[1]|주문번호[2]|원주문번호[3]|매도매수구분[4]|
///       정정구분[5]|주문종류[6]|주문조건[7]|종목코드[8]|체결수량[9]|
///       체결단가[10]|체결시간[11]|거부여부[12]|체결여부[13]|접수여부[14]|
///       지점번호[15]|주문수량[16]|계좌명[17]|...종목명[24]|주문가격[25]
pub fn parse_execution_notice(data: &DataMessage) -> Option<RealtimeData> {
    if data.data_parts.len() < 25 {
        return None;
    }

    let parts = &data.data_parts;
    let is_filled = parts.get(13).is_some_and(|v| v == "2");

    Some(RealtimeData::ExecutionNotice(ExecutionNotice {
        order_no: parts[2].clone(),
        stock_code: parts[8].clone(),
        stock_name: parts
            .get(24)
            .cloned()
            .unwrap_or_default()
            .trim()
            .to_string(),
        side: parts[4].clone(),
        filled_qty: parts[9].parse().unwrap_or(0),
        filled_price: parts[10].parse().unwrap_or(0),
        order_qty: parts[16].parse().unwrap_or(0),
        is_filled,
        timestamp: parts[11].clone(),
    }))
}

/// 장운영 정보 (H0STMKO0) 파싱 — 평문
/// 필드: 종목코드[0]|거래정지여부[1]|거래정지사유[2]|장운영구분코드[3]|
///       예상장운영구분코드[4]|임의연장구분코드[5]|동시호가배분처리구분코드[6]|
///       종목상태구분코드[7]|VI적용구분코드[8]|시간외단일가VI적용구분코드[9]
pub fn parse_market_operation(data: &DataMessage) -> Option<RealtimeData> {
    if data.data_parts.len() < 9 {
        return None;
    }

    let parts = &data.data_parts;
    Some(RealtimeData::MarketOperation(MarketOperation {
        stock_code: parts[0].clone(),
        is_trading_halt: parts[1] == "Y",
        halt_reason: parts[2].clone(),
        market_operation_code: parts[3].clone(),
        vi_applied: parts[8].clone(),
    }))
}

/// tr_id로 파이프 데이터 분기 파싱
pub fn parse_data_message(data: &DataMessage) -> Option<RealtimeData> {
    match data.tr_id.as_str() {
        "H0STCNT0" => parse_execution(data),
        "H0STASP0" => parse_orderbook(data),
        "H0STMKO0" => parse_market_operation(data),
        "H0STCNI0" | "H0STCNI9" => parse_execution_notice(data), // 복호화 후 호출 시
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_control_message() {
        let json = r#"{"header":{"tr_id":"H0STCNT0","tr_key":"005930","encrypt":"N"},"body":{"rt_cd":"0","msg_cd":"OPSP0000","msg1":"SUBSCRIBE SUCCESS"}}"#;
        let msg = parse_ws_message(json).unwrap();
        assert!(matches!(msg, WsMessage::Control(_)));
        if let WsMessage::Control(ctrl) = msg {
            assert_eq!(ctrl.header.tr_id, "H0STCNT0");
            assert_eq!(ctrl.body.rt_cd, "0");
        }
    }

    #[test]
    fn test_parse_execution_uses_official_sample_indices() {
        // KIS 공식 샘플 `legacy/Sample01/kis_domstk_ws.py`의 `contract_cols` 순서.
        // [13]은 누적거래량이라 parser는 읽지 않지만, 인접 필드 오독을 잡기 위해
        // 일부러 큰 값을 넣어 둔다.
        let pipe = concat!(
            "0|H0STCNT0|1|",
            "005930^093015^72300^2^500^0.70^72250^72000^73000^71500^72500^72400^321^123456789"
        );
        let msg = parse_ws_message(pipe).unwrap();
        assert!(matches!(msg, WsMessage::Data(_)));
        if let WsMessage::Data(data) = msg {
            assert_eq!(data.tr_id, "H0STCNT0");
            assert!(!data.encrypted);
            assert_eq!(data.data_parts[EXEC_STOCK_CODE_IDX], "005930");

            let rt = parse_execution(&data).unwrap();
            if let RealtimeData::Execution(exec) = rt {
                assert_eq!(exec.stock_code, "005930");
                assert_eq!(exec.time, "093015");
                assert_eq!(exec.price, 72300);
                assert_eq!(exec.change, 500);
                assert_eq!(exec.change_rate, 0.70);
                assert_eq!(exec.open, 72000);
                assert_eq!(exec.high, 73000);
                assert_eq!(exec.low, 71500);
                assert_eq!(exec.ask_price, 72500);
                assert_eq!(exec.bid_price, 72400);
                assert_eq!(exec.volume, 321);
            }
        }
    }

    #[test]
    fn test_parse_execution_uses_askp1_bidp1_for_spread_fields() {
        // spread gate 회귀 방지: parts[10]/[11]을 1호가로 읽어야 정상 스프레드가 나온다.
        let pipe = concat!(
            "0|H0STCNT0|1|",
            "122630^093015^108000^2^500^0.47^108010^108000^108100^107900^108050^107990^321^1234567"
        );
        let msg = parse_ws_message(pipe).unwrap();
        if let WsMessage::Data(data) = msg {
            let rt = parse_execution(&data).unwrap();
            if let RealtimeData::Execution(exec) = rt {
                assert_eq!(exec.ask_price, 108050);
                assert_eq!(exec.bid_price, 107990);
                assert_eq!(exec.volume, 321);
                assert_eq!(exec.ask_price - exec.bid_price, 60);
            }
        }
    }

    #[test]
    fn test_parse_orderbook_uses_documented_indices() {
        let mut parts = vec!["0".to_string(); ORDERBOOK_MIN_FIELDS];
        parts[ORDERBOOK_STOCK_CODE_IDX] = "122630".to_string();

        for level in 0..ORDERBOOK_LEVEL_COUNT {
            parts[ORDERBOOK_ASK_START_IDX + level] = (100_000 + level as i64 * 5).to_string();
            parts[ORDERBOOK_BID_START_IDX + level] = (99_995 - level as i64 * 5).to_string();
            parts[ORDERBOOK_ASK_QTY_START_IDX + level] = (1_000 + level as i64).to_string();
            parts[ORDERBOOK_BID_QTY_START_IDX + level] = (2_000 + level as i64).to_string();
        }
        parts[ORDERBOOK_TOTAL_ASK_IDX] = "15555".to_string();
        parts[ORDERBOOK_TOTAL_BID_IDX] = "25555".to_string();

        let data = DataMessage {
            encrypted: false,
            tr_id: "H0STASP0".to_string(),
            count: 1,
            data_parts: parts,
        };

        let rt = parse_orderbook(&data).unwrap();
        if let RealtimeData::OrderBook(book) = rt {
            assert_eq!(book.stock_code, "122630");
            assert_eq!(book.asks.len(), ORDERBOOK_LEVEL_COUNT);
            assert_eq!(book.bids.len(), ORDERBOOK_LEVEL_COUNT);
            assert_eq!(book.asks[0], (100_000, 1_000));
            assert_eq!(book.asks[9], (100_045, 1_009));
            assert_eq!(book.bids[0], (99_995, 2_000));
            assert_eq!(book.bids[9], (99_950, 2_009));
            assert_eq!(book.total_ask_volume, 15_555);
            assert_eq!(book.total_bid_volume, 25_555);
        }
    }
}
