import { useStockStore } from '../../stores/stockStore';
import { SegmentedControl, Badge } from '../ui';

const PERIODS = [
  { label: '일', value: 'D' as const },
  { label: '주', value: 'W' as const },
  { label: '월', value: 'M' as const },
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
    <div className="flex items-center gap-3 py-2 flex-wrap">
      <SegmentedControl
        options={PERIODS}
        value={chartPeriod as 'D' | 'W' | 'M'}
        onChange={(v) => setChartPeriod(v)}
        size="sm"
      />

      <div className="w-px h-5 bg-border" />

      <div className="flex gap-1 flex-wrap">
        {INDICATORS.map((ind) => {
          const active = activeIndicators.includes(ind.value);
          return (
            <button
              key={ind.value}
              onClick={() => toggleIndicator(ind.value)}
              className="focus:outline-none"
            >
              <Badge tone={active ? 'accent' : 'muted'} variant={active ? 'soft' : 'outline'} size="sm">
                {ind.label}
              </Badge>
            </button>
          );
        })}
      </div>
    </div>
  );
}
