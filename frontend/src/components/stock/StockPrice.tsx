import { useStockStore } from '../../stores/stockStore';
import { formatKRW, formatPercent, formatChange, priceColorClass, formatShortKRW } from '../../utils/format';

export function StockPrice() {
  const { priceData, selectedCode } = useStockStore();

  if (!priceData) {
    return (
      <div className="bg-card rounded-lg border border-border p-4">
        <div className="text-text-muted text-sm">종목 {selectedCode} 로딩 중...</div>
      </div>
    );
  }

  const colorClass = priceColorClass(priceData.change_sign);

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-baseline gap-2 mb-2">
        <span className="text-lg font-bold">{priceData.name || selectedCode}</span>
        <span className="text-xs text-text-muted">{selectedCode}</span>
      </div>
      <div className="flex items-baseline gap-3">
        <span className={`text-2xl font-bold ${colorClass}`}>
          {formatKRW(priceData.price)}
        </span>
        <span className={`text-sm ${colorClass}`}>
          {formatChange(priceData.change)}
        </span>
        <span className={`text-sm ${colorClass}`}>
          ({formatPercent(priceData.change_rate)})
        </span>
      </div>
      <div className="grid grid-cols-4 gap-4 mt-3 text-xs">
        <div>
          <span className="text-text-muted">시가</span>
          <div>{formatKRW(priceData.open)}</div>
        </div>
        <div>
          <span className="text-text-muted">고가</span>
          <div className="text-rise">{formatKRW(priceData.high)}</div>
        </div>
        <div>
          <span className="text-text-muted">저가</span>
          <div className="text-fall">{formatKRW(priceData.low)}</div>
        </div>
        <div>
          <span className="text-text-muted">거래대금</span>
          <div>{formatShortKRW(priceData.amount)}</div>
        </div>
      </div>
    </div>
  );
}
