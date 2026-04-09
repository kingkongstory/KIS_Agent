use chrono::NaiveTime;

/// 포지션 방향
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSide {
    Long,
    Short,
}

/// 진입 신호
#[derive(Debug, Clone)]
pub struct Signal {
    pub side: PositionSide,
    pub entry_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    pub signal_time: NaiveTime,
}

/// 보유 포지션
#[derive(Debug, Clone)]
pub struct Position {
    pub side: PositionSide,
    pub entry_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    pub entry_time: NaiveTime,
    pub quantity: u64,
    /// TP 지정가 주문번호 (None이면 시장가 fallback)
    pub tp_order_no: Option<String>,
    /// TP KRX 조직번호 (취소/정정용)
    pub tp_krx_orgno: Option<String>,
    /// 현재 걸려있는 TP 지정가
    pub tp_limit_price: Option<i64>,
}

/// 청산 사유
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitReason {
    StopLoss,
    TakeProfit,
    /// 본전 스탑 (1R 도달 후 진입가로 SL 이동 → 터치)
    BreakevenStop,
    /// 트레일링 스탑 (최고가/최저가 추적, 0.5R 간격)
    TrailingStop,
    /// 시간 스탑 (진입 후 30분 내 1R 미달)
    TimeStop,
    EndOfDay,
}

/// 거래 결과
#[derive(Debug, Clone)]
pub struct TradeResult {
    pub side: PositionSide,
    pub entry_price: i64,
    pub exit_price: i64,
    pub stop_loss: i64,
    pub take_profit: i64,
    pub entry_time: NaiveTime,
    pub exit_time: NaiveTime,
    pub exit_reason: ExitReason,
}

impl TradeResult {
    /// 손익률 (%)
    pub fn pnl_pct(&self) -> f64 {
        let diff = match self.side {
            PositionSide::Long => self.exit_price - self.entry_price,
            PositionSide::Short => self.entry_price - self.exit_price,
        };
        (diff as f64 / self.entry_price as f64) * 100.0
    }

    /// 실현 R 배수 (수익 / 리스크)
    pub fn realized_rr(&self) -> f64 {
        let risk = (self.entry_price - self.stop_loss).abs() as f64;
        if risk == 0.0 {
            return 0.0;
        }
        let pnl = match self.side {
            PositionSide::Long => (self.exit_price - self.entry_price) as f64,
            PositionSide::Short => (self.entry_price - self.exit_price) as f64,
        };
        pnl / risk
    }

    /// 승리 여부
    pub fn is_win(&self) -> bool {
        self.pnl_pct() > 0.0
    }
}
