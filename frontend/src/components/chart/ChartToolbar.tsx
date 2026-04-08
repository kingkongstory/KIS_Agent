import { useStockStore } from '../../stores/stockStore';
import { cn } from '../../utils/cn';

const PERIODS = [
  { label: '일', value: 'D' },
  { label: '주', value: 'W' },
  { label: '월', value: 'M' },
];

const INDICATORS = [
  { label: 'SMA 5', value: 'sma_5' },
  { label: 'SMA 20', value: 'sma_20' },
  { label: 'SMA 60', value: 'sma_60' },
  { label: 'EMA 12', value: 'ema_12' },
  { label: 'RSI', value: 'rsi_14' },
  { label: 'MACD', value: 'macd' },
  { label: 'BB', value: 'bb' },
];

export function ChartToolbar() {
  const { chartPeriod, setChartPeriod, activeIndicators, toggleIndicator } =
    useStockStore();

  return (
    <div className="flex items-center gap-4 py-2">
      {/* 기간 선택 */}
      <div className="flex gap-1">
        {PERIODS.map((p) => (
          <button
            key={p.value}
            onClick={() => setChartPeriod(p.value)}
            className={cn(
              'px-3 py-1 text-xs rounded transition-colors',
              chartPeriod === p.value
                ? 'bg-accent text-white'
                : 'bg-card text-text-muted hover:bg-border/50',
            )}
          >
            {p.label}
          </button>
        ))}
      </div>

      <div className="w-px h-4 bg-border" />

      {/* 지표 토글 */}
      <div className="flex gap-1 flex-wrap">
        {INDICATORS.map((ind) => (
          <button
            key={ind.value}
            onClick={() => toggleIndicator(ind.value)}
            className={cn(
              'px-2 py-1 text-xs rounded transition-colors',
              activeIndicators.includes(ind.value)
                ? 'bg-accent/20 text-accent border border-accent/30'
                : 'bg-card text-text-muted hover:bg-border/50',
            )}
          >
            {ind.label}
          </button>
        ))}
      </div>
    </div>
  );
}
