use serde::{Deserialize, Serialize};

use crate::domain::serde_utils::{string_to_f64, string_to_i64};

/// 개별 종목 보유 잔고
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionItem {
    /// 종목코드
    pub pdno: String,

    /// 종목명
    pub prdt_name: String,

    /// 보유수량
    #[serde(
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub hldg_qty: i64,

    /// 매입평균가
    #[serde(
        deserialize_with = "string_to_f64::deserialize",
        serialize_with = "string_to_f64::serialize"
    )]
    pub pchs_avg_pric: f64,

    /// 현재가
    #[serde(
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub prpr: i64,

    /// 평가손익금액
    #[serde(
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub evlu_pfls_amt: i64,

    /// 평가손익률 (%)
    #[serde(
        deserialize_with = "string_to_f64::deserialize",
        serialize_with = "string_to_f64::serialize"
    )]
    pub evlu_pfls_rt: f64,

    /// 매입금액
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub pchs_amt: i64,

    /// 평가금액
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub evlu_amt: i64,
}

/// 계좌 합계 정보
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountSummary {
    /// 예수금 총금액
    #[serde(
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub dnca_tot_amt: i64,

    /// 총평가금액
    #[serde(
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub tot_evlu_amt: i64,

    /// 평가손익 합계금액
    #[serde(
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub evlu_pfls_smtl_amt: i64,

    /// 매입금액 합계
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub pchs_amt_smtl_amt: i64,
}

/// 매수 가능 정보 (KIS inquire-psbl-order 응답)
///
/// 종목증거금율이 반영된 정확한 가능수량은 `nrcvb_buy_qty` (미수 사용 X) 또는
/// `max_buy_qty` (미수 사용 O)에서 확인. KIS 응답에는 `ord_psbl_qty` 필드가
/// 존재하지 않으므로 사용 금지.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuyableInfo {
    /// 주문가능현금
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub ord_psbl_cash: i64,

    /// 미수없는매수금액 (미수 사용 X)
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub nrcvb_buy_amt: i64,

    /// 미수없는매수수량 (미수 사용 X — 종목증거금율 반영)
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub nrcvb_buy_qty: i64,

    /// 최대매수금액 (미수 사용 O)
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub max_buy_amt: i64,

    /// 최대매수수량 (미수 사용 O)
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub max_buy_qty: i64,
}

impl BuyableInfo {
    /// 미수 사용 X 기준 매수가능수량 (우리는 모의/실전 모두 미수 사용 X)
    pub fn orderable_qty(&self) -> i64 {
        self.nrcvb_buy_qty
    }

    /// 미수 사용 X 기준 매수가능금액
    pub fn orderable_cash(&self) -> i64 {
        if self.nrcvb_buy_amt > 0 {
            self.nrcvb_buy_amt
        } else {
            self.ord_psbl_cash
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_position_item_deserialize() {
        let json = r#"{
            "pdno": "005930",
            "prdt_name": "삼성전자",
            "hldg_qty": "100",
            "pchs_avg_pric": "71500.00",
            "prpr": "72300",
            "evlu_pfls_amt": "80000",
            "evlu_pfls_rt": "1.12"
        }"#;
        let pos: PositionItem = serde_json::from_str(json).unwrap();
        assert_eq!(pos.pdno, "005930");
        assert_eq!(pos.hldg_qty, 100);
        assert!((pos.pchs_avg_pric - 71500.0).abs() < 0.01);
        assert_eq!(pos.prpr, 72300);
    }

    #[test]
    fn test_account_summary_deserialize() {
        let json = r#"{
            "dnca_tot_amt": "5000000",
            "tot_evlu_amt": "12300000",
            "evlu_pfls_smtl_amt": "300000"
        }"#;
        let summary: AccountSummary = serde_json::from_str(json).unwrap();
        assert_eq!(summary.dnca_tot_amt, 5_000_000);
        assert_eq!(summary.tot_evlu_amt, 12_300_000);
    }

    #[test]
    fn test_buyable_info_deserialize() {
        // KIS 공식 응답 필드명 (ord_psbl_qty는 존재하지 않음)
        let json = r#"{
            "ord_psbl_cash": "9837431",
            "nrcvb_buy_amt": "9837431",
            "nrcvb_buy_qty": "82",
            "max_buy_amt": "9837431",
            "max_buy_qty": "82"
        }"#;
        let info: BuyableInfo = serde_json::from_str(json).unwrap();
        assert_eq!(info.ord_psbl_cash, 9_837_431);
        assert_eq!(info.nrcvb_buy_qty, 82);
        assert_eq!(info.max_buy_qty, 82);
        assert_eq!(info.orderable_qty(), 82);
        assert_eq!(info.orderable_cash(), 9_837_431);
    }
}
