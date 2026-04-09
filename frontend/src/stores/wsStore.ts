import { create } from 'zustand';
import type {
  RealtimeExecution,
  RealtimeOrderBook,
  CandleUpdate,
  PriceSnapshot,
  BalanceSnapshot,
  MarketOperation,
} from '../types/websocket';

/** 완성된 분봉 캔들 */
export interface CandleBar {
  time: string;
  open: number;
  high: number;
  low: number;
  close: number;
  volume: number;
}

interface WsState {
  connected: boolean;
  /** 최근 체결 데이터 (종목코드별) */
  executions: Map<string, RealtimeExecution>;
  /** 실시간 호가 (종목코드별) */
  orderBooks: Map<string, RealtimeOrderBook>;
  /** 현재가 스냅샷 (종목코드별, 스케줄러 push) */
  prices: Map<string, PriceSnapshot>;
  /** 실시간 분봉 캔들 히스토리 (종목코드별) */
  candles: Map<string, CandleBar[]>;
  /** 현재 진행 중인 캔들 (종목코드별) */
  currentCandle: Map<string, CandleUpdate>;
  /** 잔고 스냅샷 (스케줄러 push) */
  balance: BalanceSnapshot | null;
  /** 잔고 갱신 트리거 (주문 체결 시 증가) */
  balanceVersion: number;
  /** 장운영 상태 (종목코드별) */
  marketStatus: Map<string, MarketOperation>;

  setConnected: (connected: boolean) => void;
  addExecution: (data: RealtimeExecution) => void;
  updateOrderBook: (data: RealtimeOrderBook) => void;
  updateCandle: (data: CandleUpdate) => void;
  updatePrice: (data: PriceSnapshot) => void;
  updateBalance: (data: BalanceSnapshot) => void;
  triggerBalanceRefresh: () => void;
  updateMarketStatus: (data: MarketOperation) => void;
}

export const useWsStore = create<WsState>((set) => ({
  connected: false,
  executions: new Map(),
  orderBooks: new Map(),
  prices: new Map(),
  candles: new Map(),
  currentCandle: new Map(),
  balance: null,
  marketStatus: new Map(),
  balanceVersion: 0,

  setConnected: (connected) => set({ connected }),

  addExecution: (data) =>
    set((state) => {
      const execMap = new Map(state.executions);
      execMap.set(data.stock_code, data);

      // 실시간 체결 → 가격 맵도 갱신 (장중 실시간 반영)
      const priceMap = new Map(state.prices);
      const prev = priceMap.get(data.stock_code);
      priceMap.set(data.stock_code, {
        type: 'PriceSnapshot' as const,
        stock_code: data.stock_code,
        name: prev?.name ?? '',
        price: data.price,
        change: data.change,
        change_sign: prev?.change_sign ?? '3',
        change_rate: data.change_rate,
        open: data.open,
        high: data.high,
        low: data.low,
        volume: prev?.volume ?? 0,  // 누적거래량은 REST에서만
        amount: prev?.amount ?? 0,  // 거래대금은 REST에서만
      });

      return { executions: execMap, prices: priceMap };
    }),

  updateOrderBook: (data) =>
    set((state) => {
      const map = new Map(state.orderBooks);
      map.set(data.stock_code, data);
      return { orderBooks: map };
    }),

  updatePrice: (data) =>
    set((state) => {
      const map = new Map(state.prices);
      map.set(data.stock_code, data);
      return { prices: map };
    }),

  updateCandle: (data) =>
    set((state) => {
      if (data.is_closed) {
        // 완성된 캔들 → 히스토리에 추가
        const candleMap = new Map(state.candles);
        const history = candleMap.get(data.stock_code) ?? [];
        // 중복 방지 (같은 시각 캔들 교체)
        const idx = history.findIndex((c) => c.time === data.time);
        const bar: CandleBar = {
          time: data.time,
          open: data.open,
          high: data.high,
          low: data.low,
          close: data.close,
          volume: data.volume,
        };
        if (idx >= 0) {
          history[idx] = bar;
        } else {
          history.push(bar);
        }
        candleMap.set(data.stock_code, history);
        // 진행 중 캔들 제거
        const currentMap = new Map(state.currentCandle);
        currentMap.delete(data.stock_code);
        return { candles: candleMap, currentCandle: currentMap };
      } else {
        // 진행 중 캔들 업데이트
        const currentMap = new Map(state.currentCandle);
        currentMap.set(data.stock_code, data);
        return { currentCandle: currentMap };
      }
    }),

  updateBalance: (data) => set({ balance: data }),

  triggerBalanceRefresh: () =>
    set((state) => ({ balanceVersion: state.balanceVersion + 1 })),

  updateMarketStatus: (data) =>
    set((state) => {
      const map = new Map(state.marketStatus);
      map.set(data.stock_code, data);
      return { marketStatus: map };
    }),
}));
