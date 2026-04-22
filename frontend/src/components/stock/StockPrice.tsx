import { useStockStore } from '../../stores/stockStore';
import {
  formatKRW,
  formatPercent,
  formatChange,
  formatShortKRW,
} from '../../utils/format';
import { Card, CardHeader, Stat, Skeleton, Badge } from '../ui';
import { cn } from '../../utils/cn';

function trendInfo(sign: string): { tone: 'rise' | 'fall' | 'muted'; arrow: string } {
  if (sign === '1' || sign === '2') return { tone: 'rise', arrow: '▲' };
  if (sign === '4' || sign === '5') return { tone: 'fall', arrow: '▼' };
  return { tone: 'muted', arrow: '–' };
}

export function StockPrice() {
  const { priceData, selectedCode } = useStockStore();

  if (!priceData) {
    return (
      <Card>
        <CardHeader title={`종목 ${selectedCode}`} subtitle="로딩 중..." />
        <Skeleton height="2.5rem" />
      </Card>
    );
  }

  const { tone, arrow } = trendInfo(priceData.change_sign);
  const colorClass = tone === 'rise' ? 'text-rise' : tone === 'fall' ? 'text-fall' : 'text-text-muted';

  return (
    <Card tone={tone === 'muted' ? 'default' : tone} padding="lg">
      <CardHeader
        title={priceData.name || selectedCode}
        subtitle={selectedCode}
        action={
          <Badge tone={tone === 'muted' ? 'muted' : tone} variant="soft" size="sm">
            {arrow} {formatPercent(priceData.change_rate)}
          </Badge>
        }
      />
      <div className="flex items-baseline gap-3">
        <span className={cn('text-4xl font-bold tabular-nums tracking-tight', colorClass)}>
          {formatKRW(priceData.price)}
        </span>
        <span className={cn('text-md tabular-nums font-medium', colorClass)}>
          {formatChange(priceData.change)}
        </span>
      </div>
      <div className="grid grid-cols-4 gap-4 mt-4 pt-4 divider-soft">
        <Stat label="시가" value={formatKRW(priceData.open)} size="md" />
        <Stat label="고가" value={formatKRW(priceData.high)} size="md" tone="rise" />
        <Stat label="저가" value={formatKRW(priceData.low)} size="md" tone="fall" />
        <Stat label="거래대금" value={formatShortKRW(priceData.amount)} size="md" />
      </div>
    </Card>
  );
}
