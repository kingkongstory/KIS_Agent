import { get } from './client';
import type { CandleData, IndicatorResult, OrderBookData, PriceData, StockSearchResult } from '../types/stock';

export function getPrice(code: string): Promise<PriceData> {
  return get<PriceData>(`/stocks/${code}/price`);
}

export function getOrderBook(code: string): Promise<OrderBookData> {
  return get<OrderBookData>(`/stocks/${code}/orderbook`);
}

export function getCandles(
  code: string,
  start: string,
  end: string,
  period = 'D',
): Promise<CandleData[]> {
  return get<CandleData[]>(
    `/stocks/${code}/candles?start=${start}&end=${end}&period=${period}`,
  );
}

export function getIndicators(
  code: string,
  names: string[],
  start: string,
  end: string,
): Promise<IndicatorResult[]> {
  return get<IndicatorResult[]>(
    `/stocks/${code}/indicators?names=${names.join(',')}&start=${start}&end=${end}`,
  );
}

export function searchStocks(query: string): Promise<StockSearchResult[]> {
  return get<StockSearchResult[]>(`/stocks/search?q=${encodeURIComponent(query)}`);
}
