import { CandleChart } from '../components/chart/CandleChart';
import { ChartToolbar } from '../components/chart/ChartToolbar';
import { OrderBook } from '../components/orderbook/OrderBook';
import { StockPrice } from '../components/stock/StockPrice';
import { useCandles } from '../hooks/useCandles';
import { useStockPrice } from '../hooks/useStockPrice';
import { useWebSocket } from '../hooks/useWebSocket';

export function DashboardPage() {
  useStockPrice();
  useCandles();
  useWebSocket();

  return (
    <div className="flex gap-4">
      {/* 메인 영역: 차트 + 현재가 */}
      <div className="flex-1 flex flex-col gap-4 min-w-0">
        <StockPrice />
        <ChartToolbar />
        <CandleChart />
      </div>
      {/* 사이드바: 호가창 */}
      <div className="w-64 shrink-0">
        <OrderBook />
      </div>
    </div>
  );
}
