import { useEffect } from 'react';
import { MinuteCandleChart } from '../components/chart/MinuteCandleChart';
import { StockPriceCard } from '../components/stock/StockPriceCard';
import { StrategyPanel } from '../components/strategy/StrategyPanel';
import { TradeLog } from '../components/strategy/TradeLog';
import { FVGPanel } from '../components/strategy/FVGPanel';
import { AccountSummary } from '../components/account/AccountSummary';
import { useStockStore, STOCK_CODES } from '../stores/stockStore';
import { useWsStore } from '../stores/wsStore';
import { get } from '../api/client';
import type { PriceData } from '../types/stock';

export function DashboardPage() {
  const { setPriceData } = useStockStore();
  const prices = useWsStore((s) => s.prices);

  // 마운트 시 REST로 초기 가격 로드 (WS PriceSnapshot 도착 전)
  useEffect(() => {
    STOCK_CODES.forEach(async (meta, i) => {
      try {
        const data = await get<PriceData>(`/stocks/${meta.code}/price`);
        setPriceData(i, data);
      } catch { /* 무시 */ }
    });
  }, [setPriceData]);

  // WebSocket PriceSnapshot → stockStore 동기화 (가격 카드에서 사용)
  useEffect(() => {
    for (let i = 0; i < STOCK_CODES.length; i++) {
      const snapshot = prices.get(STOCK_CODES[i].code);
      if (snapshot) {
        const priceData: PriceData = {
          price: snapshot.price,
          change: snapshot.change,
          change_sign: snapshot.change_sign,
          change_rate: snapshot.change_rate,
          open: snapshot.open,
          high: snapshot.high,
          low: snapshot.low,
          volume: snapshot.volume,
          amount: snapshot.amount,
          name: snapshot.name,
        };
        setPriceData(i, priceData);
      }
    }
  }, [prices, setPriceData]);

  return (
    <div className="flex flex-col gap-4 h-full">
      {/* 계좌 + 자동매매 컨트롤 + 거래 내역 */}
      <div className="grid grid-cols-3 gap-4">
        <AccountSummary />
        <StrategyPanel />
        <TradeLog />
      </div>
      {/* FVG 탐지 현황 (신호/중단/drift 이탈 실시간 요약) */}
      <FVGPanel />
      {/* 두 종목 가격 카드 */}
      <div className="grid grid-cols-2 gap-4">
        <StockPriceCard index={0} />
        <StockPriceCard index={1} />
      </div>
      {/* 실시간 분봉 차트: 1분 / 5분 / 15분 × 2종목 */}
      <div className="grid grid-cols-2 gap-3">
        <MinuteCandleChart index={0} timeframe={1} />
        <MinuteCandleChart index={1} timeframe={1} />
      </div>
      <div className="grid grid-cols-2 gap-3">
        <MinuteCandleChart index={0} timeframe={5} />
        <MinuteCandleChart index={1} timeframe={5} />
      </div>
      <div className="grid grid-cols-2 gap-3">
        <MinuteCandleChart index={0} timeframe={15} />
        <MinuteCandleChart index={1} timeframe={15} />
      </div>
    </div>
  );
}
