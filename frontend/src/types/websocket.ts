export interface RealtimeExecution {
  type: 'Execution';
  stock_code: string;
  price: number;
  volume: number;
  change: number;
  change_rate: number;
  time: string;
  ask_price: number;
  bid_price: number;
  open: number;
  high: number;
  low: number;
}

export interface RealtimeOrderBook {
  type: 'OrderBook';
  stock_code: string;
  asks: [number, number][];
  bids: [number, number][];
  total_ask_volume: number;
  total_bid_volume: number;
}

export interface CandleUpdate {
  type: 'CandleUpdate';
  stock_code: string;
  time: string; // "HH:MM"
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
  is_closed: boolean;
}

export interface TradeNotification {
  type: 'TradeNotification';
  stock_code: string;
  stock_name: string;
  action: string; // "entry" | "exit"
}

export interface PriceSnapshot {
  type: 'PriceSnapshot';
  stock_code: string;
  name: string;
  price: number;
  change: number;
  change_sign: string;
  change_rate: number;
  open: number;
  high: number;
  low: number;
  volume: number;
  amount: number;
}

export interface BalancePosition {
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

export interface BalanceSnapshot {
  type: 'BalanceSnapshot';
  positions: BalancePosition[];
  summary: {
    cash: number;
    total_eval: number;
    total_profit_loss: number;
    total_purchase: number;
  };
}

export interface ExecutionNotice {
  type: 'ExecutionNotice';
  order_no: string;
  stock_code: string;
  stock_name: string;
  side: string; // "01"=매도, "02"=매수
  filled_qty: number;
  filled_price: number;
  order_qty: number;
  is_filled: boolean;
  timestamp: string;
}

export interface MarketOperation {
  type: 'MarketOperation';
  stock_code: string;
  is_trading_halt: boolean;
  halt_reason: string;
  market_operation_code: string;
  vi_applied: string; // "0"=미적용, "1"=정적VI, "2"=동적VI, "3"=정적+동적
}

export type RealtimeMessage =
  | RealtimeExecution
  | RealtimeOrderBook
  | CandleUpdate
  | TradeNotification
  | PriceSnapshot
  | BalanceSnapshot
  | ExecutionNotice
  | MarketOperation;
