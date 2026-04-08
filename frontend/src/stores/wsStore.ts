import { create } from 'zustand';
import type { RealtimeExecution, RealtimeOrderBook } from '../types/websocket';

interface WsState {
  connected: boolean;
  /** 최근 체결 데이터 (종목코드별) */
  executions: Map<string, RealtimeExecution>;
  /** 실시간 호가 (종목코드별) */
  orderBooks: Map<string, RealtimeOrderBook>;

  setConnected: (connected: boolean) => void;
  addExecution: (data: RealtimeExecution) => void;
  updateOrderBook: (data: RealtimeOrderBook) => void;
}

export const useWsStore = create<WsState>((set) => ({
  connected: false,
  executions: new Map(),
  orderBooks: new Map(),

  setConnected: (connected) => set({ connected }),

  addExecution: (data) =>
    set((state) => {
      const map = new Map(state.executions);
      map.set(data.stock_code, data);
      return { executions: map };
    }),

  updateOrderBook: (data) =>
    set((state) => {
      const map = new Map(state.orderBooks);
      map.set(data.stock_code, data);
      return { orderBooks: map };
    }),
}));
