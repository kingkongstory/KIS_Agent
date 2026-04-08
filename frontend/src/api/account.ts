import { get } from './client';
import type { BalanceData, BuyableData } from '../types/account';
import type { ExecutionData } from '../types/order';

export function getBalance(): Promise<BalanceData> {
  return get<BalanceData>('/account/balance');
}

export function getExecutions(start: string, end: string): Promise<ExecutionData[]> {
  return get<ExecutionData[]>(`/account/executions?start=${start}&end=${end}`);
}

export function getBuyable(stockCode: string, price: number): Promise<BuyableData> {
  return get<BuyableData>(`/account/buyable?stock_code=${stockCode}&price=${price}`);
}
