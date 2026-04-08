export interface PositionData {
  stock_code: string;
  stock_name: string;
  quantity: number;
  avg_price: number;
  current_price: number;
  profit_loss: number;
  profit_loss_rate: number;
  purchase_amount: number;
  eval_amount: number;
}

export interface SummaryData {
  cash: number;
  total_eval: number;
  total_profit_loss: number;
  total_purchase: number;
}

export interface BalanceData {
  positions: PositionData[];
  summary: SummaryData;
}

export interface BuyableData {
  available_cash: number;
  available_quantity: number;
}
