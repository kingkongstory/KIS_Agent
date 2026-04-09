import { useEffect } from 'react';
import { MinuteCandleChart } from '../components/chart/MinuteCandleChart';
import { StockPriceCard } from '../components/stock/StockPriceCard';
import { StrategyPanel } from '../components/strategy/StrategyPanel';
import { TradeLog } from '../components/strategy/TradeLog';
import { AccountSummary } from '../components/account/AccountSummary';
import { useStockStore, STOCK_CODES } from '../stores/stockStore';
import { useWsStore } from '../stores/wsStore';
import type { PriceData } from '../types/stock';

export function DashboardPage() {
  const { setPriceData } = useStockStore();
  const prices = useWsStore((s) => s.prices);

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
