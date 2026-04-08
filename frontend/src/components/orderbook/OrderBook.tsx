import { useState, useEffect } from 'react';
import { getOrderBook } from '../../api/quotations';
import { useStockStore } from '../../stores/stockStore';
import { formatKRW } from '../../utils/format';
import type { OrderBookData } from '../../types/stock';

export function OrderBook() {
  const { selectedCode } = useStockStore();
  const [data, setData] = useState<OrderBookData | null>(null);

  useEffect(() => {
    if (!selectedCode) return;
    const fetch = () => {
      getOrderBook(selectedCode).then(setData).catch(() => {});
    };
    fetch();
    const interval = setInterval(fetch, 3000);
    return () => clearInterval(interval);
  }, [selectedCode]);

  if (!data) return null;

  const maxVolume = Math.max(
    ...data.asks.map((a) => a.volume),
    ...data.bids.map((b) => b.volume),
    1,
  );

  return (
    <div className="bg-card rounded-lg border border-border p-3">
      <h3 className="text-xs text-text-muted mb-2 font-semibold">호가창</h3>

      {/* 매도호가 (역순: 10→1) */}
      <div className="flex flex-col-reverse mb-1">
        {data.asks
          .filter((a) => a.price > 0)
          .slice(0, 10)
          .map((ask, i) => (
            <OrderBookRow
              key={`ask-${i}`}
              price={ask.price}
              volume={ask.volume}
              maxVolume={maxVolume}
              side="ask"
            />
          ))}
      </div>

      <div className="h-px bg-border my-1" />

      {/* 매수호가 (1→10) */}
      <div className="flex flex-col">
        {data.bids
          .filter((b) => b.price > 0)
          .slice(0, 10)
          .map((bid, i) => (
            <OrderBookRow
              key={`bid-${i}`}
              price={bid.price}
              volume={bid.volume}
              maxVolume={maxVolume}
              side="bid"
            />
          ))}
      </div>

      {/* 합계 */}
      <div className="flex justify-between text-xs mt-2 text-text-muted">
        <span className="text-fall">{formatKRW(data.total_ask_volume)}</span>
        <span className="text-rise">{formatKRW(data.total_bid_volume)}</span>
      </div>
    </div>
  );
}

function OrderBookRow({
  price,
  volume,
  maxVolume,
  side,
}: {
  price: number;
  volume: number;
  maxVolume: number;
  side: 'ask' | 'bid';
}) {
  const width = `${(volume / maxVolume) * 100}%`;
  const bgColor = side === 'ask' ? 'bg-fall/10' : 'bg-rise/10';
  const barColor = side === 'ask' ? 'bg-fall/30' : 'bg-rise/30';
  const textColor = side === 'ask' ? 'text-fall' : 'text-rise';

  return (
    <div className={`relative flex justify-between items-center px-2 py-0.5 text-xs ${bgColor}`}>
      <div
        className={`absolute ${side === 'ask' ? 'right-0' : 'left-0'} top-0 bottom-0 ${barColor}`}
        style={{ width }}
      />
      <span className={`relative ${textColor} font-medium`}>{formatKRW(price)}</span>
      <span className="relative text-text-muted">{formatKRW(volume)}</span>
    </div>
  );
}
