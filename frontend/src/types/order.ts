export interface OrderRequest {
  stock_code: string;
  side: 'buy' | 'sell';
  order_type: 'limit' | 'market';
  quantity: number;
  price?: number;
}

export interface OrderResponse {
  order_no: string;
  order_time: string;
}

export interface ExecutionData {
  date: string;
  time: string;
  order_no: string;
  side: string;
  stock_code: string;
  stock_name: string;
  quantity: number;
  price: number;
  filled_quantity: number;
  avg_price: number;
  status: string;
}
