import { useEffect, useState } from 'react';
import { MinuteCandleChart } from '../components/chart/MinuteCandleChart';
import { StockPriceCard } from '../components/stock/StockPriceCard';
import { StrategyPanel } from '../components/strategy/StrategyPanel';
import { TradeLog } from '../components/strategy/TradeLog';
import { FVGPanel } from '../components/strategy/FVGPanel';
import { AccountSummary } from '../components/account/AccountSummary';
import { useStockStore, STOCK_CODES } from '../stores/stockStore';
import { useWsStore } from '../stores/wsStore';
import { get } from '../api/client';
import { SegmentedControl } from '../components/ui';
import type { PriceData } from '../types/stock';

type Timeframe = 1 | 5 | 15;
const TIMEFRAME_OPTIONS: { value: '1' | '5' | '15'; label: string }[] = [
  { value: '1', label: '1m' },
  { value: '5', label: '5m' },
  { value: '15', label: '15m' },
];

export function DashboardPage() {
  const { setPriceData } = useStockStore();
  const prices = useWsStore((s) => s.prices);
  const [timeframe, setTimeframe] = useState<Timeframe>(5);

  useEffect(() => {
    STOCK_CODES.forEach(async (meta, i) => {
      try {
        const data = await get<PriceData>(`/stocks/${meta.code}/price`);
        setPriceData(i, data);
      } catch { /* 무시 */ }
    });
  }, [setPriceData]);

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
    <div className="flex flex-col gap-4">
      {/* Row 1: 계좌 + 자동매매 컨트롤 + 거래 내역 */}
      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        <AccountSummary />
        <StrategyPanel />
        <TradeLog />
      </div>

      {/* Row 2: FVG 탐지 현황 (full width) */}
      <FVGPanel />

      {/* Row 3: 두 종목 가격 카드 */}
      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <StockPriceCard index={0} />
        <StockPriceCard index={1} />
      </div>

      {/* Row 4+: 실시간 분봉 차트 — timeframe 토글로 압축 */}
      <div className="flex items-center justify-between">
        <h2 className="text-sm font-semibold text-text-secondary uppercase tracking-wider">
          실시간 분봉 차트
        </h2>
        <SegmentedControl
          options={TIMEFRAME_OPTIONS}
          value={String(timeframe) as '1' | '5' | '15'}
          onChange={(v) => setTimeframe(Number(v) as Timeframe)}
          size="sm"
        />
      </div>
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
        <MinuteCandleChart index={0} timeframe={timeframe} />
        <MinuteCandleChart index={1} timeframe={timeframe} />
      </div>
    </div>
  );
}
