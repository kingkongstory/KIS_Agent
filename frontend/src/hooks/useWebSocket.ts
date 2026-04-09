import { useEffect, useRef, useCallback } from 'react';
import { useWsStore } from '../stores/wsStore';
import type { RealtimeMessage } from '../types/websocket';

/** WebSocket 연결 훅 (자동 재연결) */
export function useWebSocket() {
  const wsRef = useRef<WebSocket | null>(null);
  const reconnectTimeoutRef = useRef<number | null>(null);
  const { setConnected, addExecution, updateOrderBook, updateCandle, updatePrice, updateBalance, triggerBalanceRefresh, updateMarketStatus } = useWsStore();

  const connect = useCallback(() => {
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const url = `${protocol}//${window.location.host}/api/v1/ws/realtime`;

    const ws = new WebSocket(url);
    wsRef.current = ws;

    ws.onopen = () => {
      setConnected(true);
      console.log('WebSocket 연결됨');
    };

    ws.onmessage = (event) => {
      try {
        const data: RealtimeMessage = JSON.parse(event.data);
        if (data.type === 'Execution') {
          addExecution(data);
        } else if (data.type === 'OrderBook') {
          updateOrderBook(data);
        } else if (data.type === 'CandleUpdate') {
          updateCandle(data);
        } else if (data.type === 'TradeNotification') {
          triggerBalanceRefresh();
        } else if (data.type === 'PriceSnapshot') {
          updatePrice(data);
        } else if (data.type === 'BalanceSnapshot') {
          updateBalance(data);
        } else if (data.type === 'ExecutionNotice') {
          triggerBalanceRefresh();
        } else if (data.type === 'MarketOperation') {
          updateMarketStatus(data);
        }
      } catch {
        // 파싱 실패 무시
      }
    };

    ws.onclose = () => {
      setConnected(false);
      // 3초 후 재연결
      reconnectTimeoutRef.current = window.setTimeout(connect, 3000);
    };

    ws.onerror = () => {
      ws.close();
    };
  }, [setConnected, addExecution, updateOrderBook, updateCandle, updatePrice, updateBalance, triggerBalanceRefresh, updateMarketStatus]);

  useEffect(() => {
    connect();
    return () => {
      if (wsRef.current) wsRef.current.close();
      if (reconnectTimeoutRef.current) clearTimeout(reconnectTimeoutRef.current);
    };
  }, [connect]);
}
