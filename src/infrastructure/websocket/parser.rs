use serde::Deserialize;

use crate::domain::ports::realtime::{
    ExecutionNotice, MarketOperation, RealtimeData, RealtimeExecution, RealtimeOrderBook,
};

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
    if data.data_parts.len() < 14 {
        return None;
    }

    let parts = &data.data_parts;
    Some(RealtimeData::Execution(RealtimeExecution {
        stock_code: parts[0].clone(),
        time: parts[1].clone(),
        price: parts[2].parse().unwrap_or(0),
        change: parts[4].parse::<i64>().unwrap_or(0),
        change_rate: parts[5].parse().unwrap_or(0.0),
        volume: parts[6].parse::<u64>().unwrap_or(0),
        ask_price: parts[8].parse().unwrap_or(0),
        bid_price: parts[9].parse().unwrap_or(0),
        open: parts[11].parse().unwrap_or(0),
        high: parts[12].parse().unwrap_or(0),
        low: parts[13].parse().unwrap_or(0),
    }))
}

/// 실시간 호가 (H0STASP0) 파싱
pub fn parse_orderbook(data: &DataMessage) -> Option<RealtimeData> {
    if data.data_parts.len() < 43 {
        return None;
    }

    let parts = &data.data_parts;
    let stock_code = parts[0].clone();

    let mut asks = Vec::with_capacity(10);
    let mut bids = Vec::with_capacity(10);

    // 매도호가 1-10: index 1-10
    // 매수호가 1-10: index 11-20
    // 매도잔량 1-10: index 21-30
    // 매수잔량 1-10: index 31-40
    for i in 0..10 {
        let ask_price: i64 = parts[1 + i].parse().unwrap_or(0);
        let bid_price: i64 = parts[11 + i].parse().unwrap_or(0);
        let ask_qty: i64 = parts[21 + i].parse().unwrap_or(0);
        let bid_qty: i64 = parts[31 + i].parse().unwrap_or(0);
        asks.push((ask_price, ask_qty));
        bids.push((bid_price, bid_qty));
    }

    let total_ask: i64 = parts[41].parse().unwrap_or(0);
    let total_bid: i64 = parts[42].parse().unwrap_or(0);

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
    let is_filled = parts.get(13).map_or(false, |v| v == "2");

    Some(RealtimeData::ExecutionNotice(ExecutionNotice {
        order_no: parts[2].clone(),
        stock_code: parts[8].clone(),
        stock_name: parts.get(24).cloned().unwrap_or_default().trim().to_string(),
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
    fn test_parse_pipe_message() {
        // 간략화된 파이프 데이터 (14개 필드)
        let pipe = "0|H0STCNT0|1|005930^093015^72300^2^500^0.70^150000^1082250000000^72500^72200^15000^72000^73000^71500";
        let msg = parse_ws_message(pipe).unwrap();
        assert!(matches!(msg, WsMessage::Data(_)));
        if let WsMessage::Data(data) = msg {
            assert_eq!(data.tr_id, "H0STCNT0");
            assert!(!data.encrypted);
            assert_eq!(data.data_parts[0], "005930");

            let rt = parse_execution(&data).unwrap();
            if let RealtimeData::Execution(exec) = rt {
                assert_eq!(exec.stock_code, "005930");
                assert_eq!(exec.price, 72300);
            }
        }
    }
}
