import { create } from 'zustand';
import type { CandleData, IndicatorResult, PriceData } from '../types/stock';

/** 개별 종목 상태 */
interface StockData {
  code: string;
  name: string;
  priceData: PriceData | null;
  candles: CandleData[];
  indicators: IndicatorResult[];
}

interface StockState {
  /** 두 종목 데이터 */
  stocks: [StockData, StockData];
  /** 활성 지표 */
  activeIndicators: string[];
  /** 차트 기간 */
  chartPeriod: string;
  /** 로딩 상태 */
  loading: boolean;
  /** 에러 */
  error: string | null;

  /** 하위 호환: 첫 번째 종목 코드 */
  selectedCode: string;
  priceData: PriceData | null;
  candles: CandleData[];
  indicators: IndicatorResult[];

  setPriceData: (idx: number, data: PriceData) => void;
  setCandles: (idx: number, candles: CandleData[]) => void;
  setIndicators: (idx: number, indicators: IndicatorResult[]) => void;
  toggleIndicator: (name: string) => void;
  setChartPeriod: (period: string) => void;
  setLoading: (loading: boolean) => void;
  setError: (error: string | null) => void;
  setSelectedCode: (code: string) => void;
}

export const STOCK_CODES = [
  { code: '122630', name: 'KODEX 레버리지' },
  { code: '114800', name: 'KODEX 인버스' },
] as const;

export const useStockStore = create<StockState>((set) => ({
  stocks: [
    { code: '122630', name: 'KODEX 레버리지', priceData: null, candles: [], indicators: [] },
    { code: '114800', name: 'KODEX 인버스', priceData: null, candles: [], indicators: [] },
  ],
  activeIndicators: ['sma_20'],
  chartPeriod: 'D',
  loading: false,
  error: null,

  // 하위 호환
  selectedCode: '122630',
  priceData: null,
  candles: [],
  indicators: [],

  setPriceData: (idx, data) =>
    set((state) => {
      const stocks = [...state.stocks] as [StockData, StockData];
      stocks[idx] = { ...stocks[idx], priceData: data };
      return { stocks };
    }),
  setCandles: (idx, candles) =>
    set((state) => {
      const stocks = [...state.stocks] as [StockData, StockData];
      stocks[idx] = { ...stocks[idx], candles };
      return { stocks };
    }),
  setIndicators: (idx, indicators) =>
    set((state) => {
      const stocks = [...state.stocks] as [StockData, StockData];
      stocks[idx] = { ...stocks[idx], indicators };
      return { stocks };
    }),
  toggleIndicator: (name) =>
    set((state) => {
      const active = state.activeIndicators.includes(name)
        ? state.activeIndicators.filter((n) => n !== name)
        : [...state.activeIndicators, name];
      return { activeIndicators: active };
    }),
  setChartPeriod: (period) => set({ chartPeriod: period }),
  setLoading: (loading) => set({ loading }),
  setError: (error) => set({ error }),
  setSelectedCode: () => {},
}));
