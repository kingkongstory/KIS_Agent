import { useEffect, useState, useCallback } from 'react';
import { get } from '../../api/client';
import { Card, CardHeader, Badge, EmptyState } from '../ui';

interface Trade {
  id: number;
  stock_code: string;
  stock_name: string;
  side: string;
  quantity: number;
  entry_price: number;
  exit_price: number;
  exit_reason: string;
  pnl_pct: number;
  entry_time: string;
  exit_time: string;
}

function exitReasonBadge(reason: string) {
  if (reason === 'TakeProfit') return <Badge tone="rise" variant="soft" size="xs">TP</Badge>;
  if (reason === 'StopLoss') return <Badge tone="fall" variant="soft" size="xs">SL</Badge>;
  if (reason === 'Trailing') return <Badge tone="accent" variant="soft" size="xs">TS</Badge>;
  if (reason === 'TimeStop') return <Badge tone="warn" variant="soft" size="xs">시간</Badge>;
  if (reason === 'Breakeven') return <Badge tone="muted" variant="soft" size="xs">BE</Badge>;
  return <Badge tone="muted" variant="soft" size="xs">{reason}</Badge>;
}

export function TradeLog() {
  const [trades, setTrades] = useState<Trade[]>([]);

  const fetchTrades = useCallback(async () => {
    try {
      const data = await get<Trade[]>('/strategy/trades');
      setTrades(data);
    } catch { /* 무시 */ }
  }, []);

  useEffect(() => {
    fetchTrades();
    const interval = setInterval(fetchTrades, 5000);
    return () => clearInterval(interval);
  }, [fetchTrades]);

  const totalPnl = trades.reduce((sum, t) => sum + t.pnl_pct, 0);
  const wins = trades.filter((t) => t.pnl_pct > 0).length;
  const losses = trades.filter((t) => t.pnl_pct < 0).length;

  return (
    <Card padding="md" className="overflow-hidden">
      <CardHeader
        title="거래 내역"
        subtitle="당일 체결"
        action={
          <div className="flex items-center gap-1.5">
            <Badge tone="muted" variant="outline" size="xs">{trades.length}건</Badge>
            {wins > 0 && <Badge tone="rise" variant="soft" size="xs">{wins}승</Badge>}
            {losses > 0 && <Badge tone="fall" variant="soft" size="xs">{losses}패</Badge>}
            {trades.length > 0 && (
              <Badge tone={totalPnl >= 0 ? 'rise' : 'fall'} variant="soft" size="xs">
                {totalPnl >= 0 ? '+' : ''}{totalPnl.toFixed(2)}%
              </Badge>
            )}
          </div>
        }
      />

      {trades.length === 0 ? (
        <EmptyState
          size="sm"
          title="거래 없음"
          description="조건 충족 시 자동 발주됩니다"
        />
      ) : (
        <div className="max-h-56 overflow-y-auto -mx-4 -mb-4">
          <table className="w-full text-xs tabular-nums">
            <thead className="text-2xs text-text-muted uppercase tracking-wider sticky top-0 bg-surface backdrop-blur z-10">
              <tr>
                <th className="text-left font-medium pl-4 py-2">시각</th>
                <th className="text-left font-medium px-2 py-2">종목</th>
                <th className="text-right font-medium px-2 py-2">수량</th>
                <th className="text-right font-medium px-2 py-2">진입</th>
                <th className="text-right font-medium px-2 py-2">청산</th>
                <th className="text-left font-medium px-2 py-2">사유</th>
                <th className="text-right font-medium pr-4 py-2">손익</th>
              </tr>
            </thead>
            <tbody>
              {trades.map((t) => (
                <tr key={t.id} className="border-t border-border-subtle hover:bg-surface-3/50 transition-colors">
                  <td className="pl-4 py-1.5 text-text-muted font-mono">
                    {t.entry_time.slice(11, 16)}
                  </td>
                  <td className="px-2 py-1.5">{t.stock_name.slice(0, 10)}</td>
                  <td className="px-2 py-1.5 text-right text-text-muted">
                    {t.quantity.toLocaleString()}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {t.entry_price.toLocaleString()}
                  </td>
                  <td className="px-2 py-1.5 text-right">
                    {t.exit_price.toLocaleString()}
                  </td>
                  <td className="px-2 py-1.5">{exitReasonBadge(t.exit_reason)}</td>
                  <td
                    className={`pr-4 py-1.5 text-right font-semibold ${t.pnl_pct >= 0 ? 'text-rise' : 'text-fall'}`}
                  >
                    {t.pnl_pct >= 0 ? '+' : ''}
                    {t.pnl_pct.toFixed(2)}%
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
