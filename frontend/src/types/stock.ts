export interface PriceData {
  price: number;
  change: number;
  change_sign: string;
  change_rate: number;
  open: number;
  high: number;
  low: number;
  volume: number;
  amount: number;
  name: string;
}

export interface CandleData {
  date: string;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

export interface OrderBookEntry {
  price: number;
  volume: number;
}

export interface OrderBookData {
  asks: OrderBookEntry[];
  bids: OrderBookEntry[];
  total_ask_volume: number;
  total_bid_volume: number;
}

export interface StockSearchResult {
  stock_code: string;
  stock_name: string;
  market: string;
}

export interface IndicatorPoint {
  date: string;
  values: Record<string, number>;
}

export interface IndicatorResult {
  name: string;
  values: IndicatorPoint[];
}
