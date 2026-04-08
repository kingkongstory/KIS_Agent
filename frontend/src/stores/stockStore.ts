import { create } from 'zustand';
import type { CandleData, IndicatorResult, PriceData } from '../types/stock';

interface StockState {
  /** 현재 선택된 종목코드 */
  selectedCode: string;
  /** 현재가 데이터 */
  priceData: PriceData | null;
  /** 캔들 데이터 */
  candles: CandleData[];
  /** 지표 데이터 */
  indicators: IndicatorResult[];
  /** 활성 지표 목록 */
  activeIndicators: string[];
  /** 차트 기간 */
  chartPeriod: string;
  /** 로딩 상태 */
  loading: boolean;
  /** 에러 메시지 */
  error: string | null;

  setSelectedCode: (code: string) => void;
  setPriceData: (data: PriceData) => void;
  setCandles: (candles: CandleData[]) => void;
  setIndicators: (indicators: IndicatorResult[]) => void;
  toggleIndicator: (name: string) => void;
  setChartPeriod: (period: string) => void;
  setLoading: (loading: boolean) => void;
  setError: (error: string | null) => void;
}

export const useStockStore = create<StockState>((set) => ({
  selectedCode: '005930',
  priceData: null,
  candles: [],
  indicators: [],
  activeIndicators: ['sma_20'],
  chartPeriod: 'D',
  loading: false,
  error: null,

  setSelectedCode: (code) => set({ selectedCode: code, error: null }),
  setPriceData: (data) => set({ priceData: data }),
  setCandles: (candles) => set({ candles }),
  setIndicators: (indicators) => set({ indicators }),
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
}));
