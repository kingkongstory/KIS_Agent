import { useEffect, useRef } from 'react';
import { get } from '../../api/client';
import { useWsStore } from '../../stores/wsStore';
import { formatKRW } from '../../utils/format';
import type { BalanceSnapshot } from '../../types/websocket';
import { Card, CardHeader, Stat, Skeleton, Badge } from '../ui';

const STOCKS = [
  { code: '122630', name: 'KODEX 레버리지' },
  { code: '114800', name: 'KODEX 인버스' },
];

export function AccountSummary() {
  const balance = useWsStore((s) => s.balance);
  const balanceVersion = useWsStore((s) => s.balanceVersion);
  const updateBalance = useWsStore((s) => s.updateBalance);
  const prevVersion = useRef(balanceVersion);

  useEffect(() => {
    (async () => {
      try {
        const data = await get<BalanceSnapshot>('/account/balance');
        updateBalance({ ...data, type: 'BalanceSnapshot' });
      } catch { /* 무시 */ }
    })();
  }, [updateBalance]);

  useEffect(() => {
    if (balanceVersion !== prevVersion.current) {
      prevVersion.current = balanceVersion;
      const timer = setTimeout(async () => {
        try {
          const data = await get<BalanceSnapshot>('/account/balance');
          updateBalance({ ...data, type: 'BalanceSnapshot' });
        } catch { /* 무시 */ }
      }, 1000);
      return () => clearTimeout(timer);
    }
  }, [balanceVersion, updateBalance]);

  if (!balance) {
    return (
      <Card>
        <CardHeader title="모의투자 계좌" subtitle="자산 현황" />
        <div className="grid grid-cols-3 gap-3">
          <Skeleton height="2rem" />
          <Skeleton height="2rem" />
          <Skeleton height="2rem" />
        </div>
      </Card>
    );
  }

  const s = balance.summary;
  const pnlTone = s.total_profit_loss > 0 ? 'rise' : s.total_profit_loss < 0 ? 'fall' : 'default';
  const pnlSign = s.total_profit_loss > 0 ? '+' : '';
  const posMap = new Map(balance.positions.map((p) => [p.stock_code, p]));

  return (
    <Card>
      <CardHeader
        title="모의투자 계좌"
        subtitle="전액 투입 · 선착순"
        action={<Badge tone="accent" variant="soft" size="xs">실시간</Badge>}
      />

      <div className="grid grid-cols-3 gap-3">
        <Stat label="총 자산" value={formatKRW(s.total_eval)} size="lg" />
        <Stat label="예수금" value={formatKRW(s.cash)} size="md" tone="muted" />
        <Stat
          label="평가 손익"
          value={`${pnlSign}${formatKRW(s.total_profit_loss)}`}
          size="md"
          tone={pnlTone}
        />
      </div>

      {/* 종목별 배분 */}
      <div className="mt-3 pt-3 divider-soft grid grid-cols-2 gap-3">
        {STOCKS.map(({ code, name }) => {
          const pos = posMap.get(code);
          const tone = !pos
            ? 'muted'
            : pos.profit_loss > 0
              ? 'rise'
              : pos.profit_loss < 0
                ? 'fall'
                : 'default';
          return (
            <div key={code} className="flex flex-col gap-1 min-w-0">
              <div className="flex items-center justify-between">
                <span className="text-2xs text-text-muted truncate uppercase tracking-wider font-medium">
                  {name}
                </span>
                <Badge tone={pos ? 'accent' : 'muted'} variant="outline" size="xs">
                  {pos ? '보유' : '대기'}
                </Badge>
              </div>
              {pos ? (
                <div className="flex items-center justify-between text-xs">
                  <span className="text-text-muted tabular-nums">
                    {pos.quantity}주 @ {pos.avg_price.toLocaleString()}
                  </span>
                  <span className={`tabular-nums font-medium ${tone === 'rise' ? 'text-rise' : tone === 'fall' ? 'text-fall' : 'text-text-muted'}`}>
                    {pos.profit_loss > 0 ? '+' : ''}
                    {formatKRW(pos.profit_loss)}{' '}
                    <span className="opacity-70">({pos.profit_loss_rate.toFixed(2)}%)</span>
                  </span>
                </div>
              ) : (
                <span className="text-xs text-text-disabled">진입 대기 중</span>
              )}
            </div>
          );
        })}
      </div>
    </Card>
  );
}
