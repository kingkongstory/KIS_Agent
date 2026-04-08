import { useStockStore, STOCK_CODES } from '../../stores/stockStore';
import { formatKRW, formatPercent, formatChange, priceColorClass, formatShortKRW } from '../../utils/format';

interface Props {
  index: number;
}

export function StockPriceCard({ index }: Props) {
  const stock = useStockStore((s) => s.stocks[index]);
  const meta = STOCK_CODES[index];

  if (!stock.priceData) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <div className="text-text-muted text-sm">{meta.name} 로딩 중...</div>
      </div>
    );
  }

  const p = stock.priceData;
  const colorClass = priceColorClass(p.change_sign);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-baseline gap-2 mb-2">
        <span className="text-lg font-bold">{meta.name}</span>
        <span className="text-xs text-text-muted">{meta.code}</span>
      </div>
      <div className="flex items-baseline gap-3">
        <span className={`text-2xl font-bold ${colorClass}`}>
          {formatKRW(p.price)}
        </span>
        <span className={`text-sm ${colorClass}`}>
          {formatChange(p.change)}
        </span>
        <span className={`text-sm ${colorClass}`}>
          ({formatPercent(p.change_rate)})
        </span>
      </div>
      <div className="grid grid-cols-4 gap-4 mt-3 text-xs">
        <div>
          <span className="text-text-muted">시가</span>
          <div>{formatKRW(p.open)}</div>
        </div>
        <div>
          <span className="text-text-muted">고가</span>
          <div className="text-rise">{formatKRW(p.high)}</div>
        </div>
        <div>
          <span className="text-text-muted">저가</span>
          <div className="text-fall">{formatKRW(p.low)}</div>
        </div>
        <div>
          <span className="text-text-muted">거래대금</span>
          <div>{formatShortKRW(p.amount)}</div>
        </div>
      </div>
    </div>
  );
}
