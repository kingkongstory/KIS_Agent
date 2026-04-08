use serde::{Deserialize, Serialize};

use crate::domain::serde_utils::{string_to_f64, string_to_i64};

/// 주식 현재가 조회 응답
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InquirePrice {
    /// 주식 현재가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    #[serde(serialize_with = "string_to_i64::serialize")]
    pub stck_prpr: i64,

    /// 전일 대비 (절대값)
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    #[serde(serialize_with = "string_to_i64::serialize")]
    pub prdy_vrss: i64,

    /// 전일 대비 부호 ("1"~"5")
    pub prdy_vrss_sign: String,

    /// 전일 대비율 (%)
    #[serde(deserialize_with = "string_to_f64::deserialize")]
    #[serde(serialize_with = "string_to_f64::serialize")]
    pub prdy_ctrt: f64,

    /// 시가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    #[serde(serialize_with = "string_to_i64::serialize")]
    pub stck_oprc: i64,

    /// 고가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    #[serde(serialize_with = "string_to_i64::serialize")]
    pub stck_hgpr: i64,

    /// 저가
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    #[serde(serialize_with = "string_to_i64::serialize")]
    pub stck_lwpr: i64,

    /// 누적 거래량
    #[serde(deserialize_with = "string_to_i64::deserialize")]
    #[serde(serialize_with = "string_to_i64::serialize")]
    pub acml_vol: i64,

    /// 누적 거래대금
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub acml_tr_pbmn: i64,

    /// 종목명
    #[serde(default)]
    pub hts_kor_isnm: String,
}

/// 호가 (10단계 매도/매수)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskingPrice {
    // 매도호가 1~10
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp1: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp2: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp3: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp4: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp5: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp6: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp7: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp8: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp9: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp10: i64,

    // 매수호가 1~10
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp1: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp2: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp3: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp4: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp5: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp6: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp7: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp8: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp9: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp10: i64,

    // 매도호가 잔량 1~10
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn1: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn2: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn3: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn4: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn5: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn6: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn7: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn8: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn9: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub askp_rsqn10: i64,

    // 매수호가 잔량 1~10
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn1: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn2: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn3: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn4: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn5: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn6: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn7: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn8: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn9: i64,
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub bidp_rsqn10: i64,

    /// 총 매도호가 잔량
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub total_askp_rsqn: i64,

    /// 총 매수호가 잔량
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub total_bidp_rsqn: i64,
}

impl AskingPrice {
    /// 매도호가 배열 반환 (1~10호가, 가격+잔량)
    pub fn asks(&self) -> Vec<(i64, i64)> {
        vec![
            (self.askp1, self.askp_rsqn1),
            (self.askp2, self.askp_rsqn2),
            (self.askp3, self.askp_rsqn3),
            (self.askp4, self.askp_rsqn4),
            (self.askp5, self.askp_rsqn5),
            (self.askp6, self.askp_rsqn6),
            (self.askp7, self.askp_rsqn7),
            (self.askp8, self.askp_rsqn8),
            (self.askp9, self.askp_rsqn9),
            (self.askp10, self.askp_rsqn10),
        ]
    }

    /// 매수호가 배열 반환 (1~10호가, 가격+잔량)
    pub fn bids(&self) -> Vec<(i64, i64)> {
        vec![
            (self.bidp1, self.bidp_rsqn1),
            (self.bidp2, self.bidp_rsqn2),
            (self.bidp3, self.bidp_rsqn3),
            (self.bidp4, self.bidp_rsqn4),
            (self.bidp5, self.bidp_rsqn5),
            (self.bidp6, self.bidp_rsqn6),
            (self.bidp7, self.bidp_rsqn7),
            (self.bidp8, self.bidp_rsqn8),
            (self.bidp9, self.bidp_rsqn9),
            (self.bidp10, self.bidp_rsqn10),
        ]
    }
}

/// 기간별 시세 항목 (OHLCV)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyPriceItem {
    /// 영업일자
    #[serde(deserialize_with = "crate::domain::serde_utils::kis_date::deserialize")]
    #[serde(serialize_with = "crate::domain::serde_utils::kis_date::serialize")]
    pub stck_bsop_date: chrono::NaiveDate,

    /// 시가
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub stck_oprc: i64,

    /// 고가
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub stck_hgpr: i64,

    /// 저가
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub stck_lwpr: i64,

    /// 종가
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub stck_clpr: i64,

    /// 누적 거래량
    #[serde(deserialize_with = "string_to_i64::deserialize", serialize_with = "string_to_i64::serialize")]
    pub acml_vol: i64,

    /// 누적 거래대금
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub acml_tr_pbmn: i64,

    /// 전일 대비
    #[serde(
        default,
        deserialize_with = "string_to_i64::deserialize",
        serialize_with = "string_to_i64::serialize"
    )]
    pub prdy_vrss: i64,

    /// 전일 대비 부호
    #[serde(default)]
    pub prdy_vrss_sign: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inquire_price_deserialize() {
        let json = r#"{
            "stck_prpr": "72300",
            "prdy_vrss": "500",
            "prdy_vrss_sign": "2",
            "prdy_ctrt": "0.70",
            "stck_oprc": "72000",
            "stck_hgpr": "73000",
            "stck_lwpr": "71500",
            "acml_vol": "15000000",
            "acml_tr_pbmn": "1082250000000",
            "hts_kor_isnm": "삼성전자"
        }"#;
        let price: InquirePrice = serde_json::from_str(json).unwrap();
        assert_eq!(price.stck_prpr, 72300);
        assert_eq!(price.prdy_vrss, 500);
        assert_eq!(price.stck_oprc, 72000);
        assert_eq!(price.hts_kor_isnm, "삼성전자");
    }

    #[test]
    fn test_daily_price_item_deserialize() {
        let json = r#"{
            "stck_bsop_date": "20250115",
            "stck_oprc": "72000",
            "stck_hgpr": "73000",
            "stck_lwpr": "71500",
            "stck_clpr": "72300",
            "acml_vol": "15000000"
        }"#;
        let item: DailyPriceItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.stck_clpr, 72300);
        assert_eq!(
            item.stck_bsop_date,
            chrono::NaiveDate::from_ymd_opt(2025, 1, 15).unwrap()
        );
    }
}
