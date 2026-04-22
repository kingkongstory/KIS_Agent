import { useStockStore, STOCK_CODES } from '../../stores/stockStore';
import {
  formatKRW,
  formatPercent,
  formatChange,
  formatShortKRW,
} from '../../utils/format';
import { Card, CardHeader, Stat, Skeleton, Badge } from '../ui';
import { cn } from '../../utils/cn';

interface Props {
  index: number;
}

function trendInfo(sign: string): { tone: 'rise' | 'fall' | 'muted'; arrow: string } {
  if (sign === '1' || sign === '2') return { tone: 'rise', arrow: '▲' };
  if (sign === '4' || sign === '5') return { tone: 'fall', arrow: '▼' };
  return { tone: 'muted', arrow: '–' };
}

export function StockPriceCard({ index }: Props) {
  const stock = useStockStore((s) => s.stocks[index]);
  const meta = STOCK_CODES[index];

  if (!stock.priceData) {
    return (
      <Card>
        <CardHeader title={meta.name} subtitle={meta.code} />
        <Skeleton height="2.5rem" />
        <div className="grid grid-cols-4 gap-3 mt-3">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} height="1.25rem" />
          ))}
        </div>
      </Card>
    );
  }

  const p = stock.priceData;
  const { tone, arrow } = trendInfo(p.change_sign);
  const colorClass = tone === 'rise' ? 'text-rise' : tone === 'fall' ? 'text-fall' : 'text-text-muted';

  return (
    <Card tone={tone === 'muted' ? 'default' : tone}>
      <CardHeader
        title={meta.name}
        subtitle={meta.code}
        action={
          <Badge tone={tone === 'muted' ? 'muted' : tone} variant="soft" size="sm">
            {arrow} {formatPercent(p.change_rate)}
          </Badge>
        }
      />

      <div className="flex items-baseline gap-3">
        <span className={cn('text-3xl font-bold tabular-nums tracking-tight', colorClass)}>
          {formatKRW(p.price)}
        </span>
        <span className={cn('text-sm tabular-nums font-medium', colorClass)}>
          {formatChange(p.change)}
        </span>
      </div>

      <div className="grid grid-cols-4 gap-3 mt-4 pt-3 divider-soft">
        <Stat label="시가" value={formatKRW(p.open)} size="sm" />
        <Stat label="고가" value={formatKRW(p.high)} size="sm" tone="rise" />
        <Stat label="저가" value={formatKRW(p.low)} size="sm" tone="fall" />
        <Stat label="거래대금" value={formatShortKRW(p.amount)} size="sm" />
      </div>
    </Card>
  );
}
