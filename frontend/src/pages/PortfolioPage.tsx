import { useState, useEffect } from 'react';
import { getBalance } from '../api/account';
import type { BalanceData } from '../types/account';
import { formatKRW, formatPercent } from '../utils/format';
import { Card, CardHeader, Stat, EmptyState, Skeleton } from '../components/ui';
import { cn } from '../utils/cn';

export function PortfolioPage() {
  const [balance, setBalance] = useState<BalanceData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getBalance()
      .then(setBalance)
      .catch((e) => setError(e.message));
  }, []);

  if (error) {
    return (
      <Card>
        <EmptyState title="계좌 조회 실패" description={error} />
      </Card>
    );
  }

  if (!balance) {
    return (
      <Card>
        <CardHeader title="포트폴리오" subtitle="로딩 중..." />
        <div className="grid grid-cols-4 gap-4">
          {Array.from({ length: 4 }).map((_, i) => (
            <Skeleton key={i} height="2.5rem" />
          ))}
        </div>
      </Card>
    );
  }

  const s = balance.summary;
  const pnlTone = s.total_profit_loss > 0 ? 'rise' : s.total_profit_loss < 0 ? 'fall' : 'default';

  return (
    <div className="flex flex-col gap-4">
      <Card>
        <CardHeader title="계좌 요약" subtitle="모의투자" />
        <div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
          <Stat label="예수금" value={formatKRW(s.cash)} size="lg" />
          <Stat label="총평가" value={formatKRW(s.total_eval)} size="lg" />
          <Stat label="총매입" value={formatKRW(s.total_purchase)} size="lg" tone="muted" />
          <Stat
            label="총손익"
            value={`${s.total_profit_loss > 0 ? '+' : ''}${formatKRW(s.total_profit_loss)}`}
            size="lg"
            tone={pnlTone}
          />
        </div>
      </Card>

      <Card padding="none" className="overflow-hidden">
        <div className="px-4 py-3 border-b border-border-subtle">
          <div className="text-sm font-semibold">보유 종목</div>
        </div>
        {balance.positions.length === 0 ? (
          <EmptyState title="보유 종목 없음" description="포지션이 진입되면 여기에 표시됩니다" />
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm tabular-nums">
              <thead className="text-2xs text-text-muted uppercase tracking-wider bg-surface-2">
                <tr>
                  <th className="text-left font-medium px-4 py-2.5">종목</th>
                  <th className="text-right font-medium px-4 py-2.5">수량</th>
                  <th className="text-right font-medium px-4 py-2.5">평균가</th>
                  <th className="text-right font-medium px-4 py-2.5">현재가</th>
                  <th className="text-right font-medium px-4 py-2.5">손익</th>
                  <th className="text-right font-medium px-4 py-2.5">수익률</th>
                </tr>
              </thead>
              <tbody>
                {balance.positions.map((pos) => {
                  const tone =
                    pos.profit_loss > 0
                      ? 'text-rise'
                      : pos.profit_loss < 0
                        ? 'text-fall'
                        : 'text-text-muted';
                  return (
                    <tr
                      key={pos.stock_code}
                      className="border-t border-border-subtle hover:bg-surface-3/50 transition-colors"
                    >
                      <td className="px-4 py-3">
                        <div className="font-medium text-text-primary">{pos.stock_name}</div>
                        <div className="text-xs text-text-muted font-mono">{pos.stock_code}</div>
                      </td>
                      <td className="text-right px-4 py-3">{formatKRW(pos.quantity)}</td>
                      <td className="text-right px-4 py-3 text-text-muted">
                        {formatKRW(Math.round(pos.avg_price))}
                      </td>
                      <td className="text-right px-4 py-3">{formatKRW(pos.current_price)}</td>
                      <td className={cn('text-right px-4 py-3 font-medium', tone)}>
                        {pos.profit_loss > 0 ? '+' : ''}
                        {formatKRW(pos.profit_loss)}
                      </td>
                      <td className={cn('text-right px-4 py-3 font-semibold', tone)}>
                        {formatPercent(pos.profit_loss_rate)}
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </div>
  );
}
