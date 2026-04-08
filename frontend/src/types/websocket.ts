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

export type RealtimeMessage = RealtimeExecution | RealtimeOrderBook;
