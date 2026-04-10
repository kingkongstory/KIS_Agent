use serde::Serialize;

use crate::domain::models::account::{AccountSummary, BuyableInfo, PositionItem};
use crate::domain::models::order::ExecutionItem;

/// 잔고 응답 DTO
#[derive(Debug, Serialize)]
pub struct BalanceDto {
    pub positions: Vec<PositionDto>,
    pub summary: SummaryDto,
}

/// 보유 종목 DTO
#[derive(Debug, Serialize)]
pub struct PositionDto {
    pub stock_code: String,
    pub stock_name: String,
    pub quantity: i64,
    pub avg_price: f64,
    pub current_price: i64,
    pub profit_loss: i64,
    pub profit_loss_rate: f64,
    pub purchase_amount: i64,
    pub eval_amount: i64,
}

impl From<PositionItem> for PositionDto {
    fn from(p: PositionItem) -> Self {
        Self {
            stock_code: p.pdno,
            stock_name: p.prdt_name,
            quantity: p.hldg_qty,
            avg_price: p.pchs_avg_pric,
            current_price: p.prpr,
            profit_loss: p.evlu_pfls_amt,
            profit_loss_rate: p.evlu_pfls_rt,
            purchase_amount: p.pchs_amt,
            eval_amount: p.evlu_amt,
        }
    }
}

/// 계좌 합계 DTO
#[derive(Debug, Serialize)]
pub struct SummaryDto {
    pub cash: i64,
    pub total_eval: i64,
    pub total_profit_loss: i64,
    pub total_purchase: i64,
}

impl From<AccountSummary> for SummaryDto {
    fn from(s: AccountSummary) -> Self {
        Self {
            cash: s.dnca_tot_amt,
            total_eval: s.tot_evlu_amt,
            total_profit_loss: s.evlu_pfls_smtl_amt,
            total_purchase: s.pchs_amt_smtl_amt,
        }
    }
}

/// 체결 내역 DTO
#[derive(Debug, Serialize)]
pub struct ExecutionDto {
    pub date: String,
    pub time: String,
    pub order_no: String,
    pub side: String,
    pub stock_code: String,
    pub stock_name: String,
    pub quantity: i64,
    pub price: i64,
    pub filled_quantity: i64,
    pub avg_price: f64,
    pub status: String,
}

impl From<ExecutionItem> for ExecutionDto {
    fn from(e: ExecutionItem) -> Self {
        Self {
            date: e
                .ord_dt
                .map(|d| d.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            time: e.ord_tmd,
            order_no: e.odno,
            side: e.sll_buy_dvsn_cd_name,
            stock_code: e.pdno,
            stock_name: e.prdt_name,
            quantity: e.ord_qty,
            price: e.ord_unpr,
            filled_quantity: e.tot_ccld_qty,
            avg_price: e.avg_prvs,
            status: e.ccld_cndt_name,
        }
    }
}

/// 매수 가능 DTO
#[derive(Debug, Serialize)]
pub struct BuyableDto {
    pub available_cash: i64,
    pub available_quantity: i64,
}

impl From<BuyableInfo> for BuyableDto {
    fn from(b: BuyableInfo) -> Self {
        Self {
            available_cash: b.orderable_cash(),
            available_quantity: b.orderable_qty(),
        }
    }
}
