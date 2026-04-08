import { useEffect, useCallback } from 'react';
import { CandleChart } from '../components/chart/CandleChart';
import { StockPriceCard } from '../components/stock/StockPriceCard';
import { StrategyPanel } from '../components/strategy/StrategyPanel';
import { AccountSummary } from '../components/account/AccountSummary';
import { useStockStore, STOCK_CODES } from '../stores/stockStore';
import { getPrice, getCandles } from '../api/quotations';

export function DashboardPage() {
  const { setPriceData, setCandles, chartPeriod, setError } = useStockStore();

  const fetchAll = useCallback(async () => {
    for (let i = 0; i < STOCK_CODES.length; i++) {
      const { code } = STOCK_CODES[i];
      try {
        const price = await getPrice(code);
        setPriceData(i, price);
      } catch (e) {
        setError(e instanceof Error ? e.message : '조회 실패');
      }
    }
  }, [setPriceData, setError]);

  const fetchCandles = useCallback(async () => {
    const end = new Date().toISOString().slice(0, 10).replace(/-/g, '');
    const start = new Date(Date.now() - 90 * 86400000).toISOString().slice(0, 10).replace(/-/g, '');
    for (let i = 0; i < STOCK_CODES.length; i++) {
      try {
        const candles = await getCandles(STOCK_CODES[i].code, start, end, chartPeriod);
        setCandles(i, candles);
      } catch {
        // 캔들 로드 실패 무시
      }
    }
  }, [setCandles, chartPeriod]);

  useEffect(() => {
    fetchAll();
    const interval = setInterval(fetchAll, 5000);
    return () => clearInterval(interval);
  }, [fetchAll]);

  useEffect(() => {
    fetchCandles();
  }, [fetchCandles]);

  return (
    <div className="flex flex-col gap-4 h-full">
      {/* 계좌 + 자동매매 컨트롤 */}
      <div className="grid grid-cols-3 gap-4">
        <AccountSummary />
        <div className="col-span-2">
          <StrategyPanel />
        </div>
      </div>
      {/* 두 종목 가격 카드 */}
      <div className="grid grid-cols-2 gap-4">
        <StockPriceCard index={0} />
        <StockPriceCard index={1} />
      </div>
      {/* 두 종목 차트 */}
      <div className="grid grid-cols-2 gap-4 flex-1 min-h-0">
        <CandleChart index={0} />
        <CandleChart index={1} />
      </div>
    </div>
  );
}
