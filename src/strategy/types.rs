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
    /// 1R 도달 여부 (트레일링/본전스탑 활성화 판별)
    pub reached_1r: bool,
    /// 유리한 방향 최고/최저가 (트레일링 SL 계산 기준)
    pub best_price: i64,
    /// 원래 SL (1R 이전 기준, ExitReason 판정용)
    pub original_sl: i64,
    /// WS 틱 리스너가 SL 돌파를 감지했을 때 설정되는 플래그.
    /// `manage_position` 이 이 플래그를 보고 TP 체크를 건너뛰고
    /// 즉시 시장가 청산 경로로 진입. 포지션 청산 후 position 자체가
    /// drop 되면서 자동 리셋.
    pub sl_triggered_tick: bool,
    /// 이론 진입가 (FVG mid_price — 슬리피지 = entry_price - intended_entry_price)
    pub intended_entry_price: i64,
    /// 주문 → 체결 확인 지연 (밀리초)
    pub order_to_fill_ms: i64,
    /// 진입을 발생시킨 신호 식별자. execution_journal 상관관계 키로 사용.
    pub signal_id: String,
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
    /// 거래된 실제 수량. 백테스트에서는 0(기본).
    pub quantity: u64,
    /// 이론 진입가 (슬리피지 계산용, 백테스트는 entry_price와 동일)
    pub intended_entry_price: i64,
    /// 주문 → 체결 확인 지연 (밀리초, 백테스트는 0)
    pub order_to_fill_ms: i64,
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
