import { useEffect, useState, useCallback } from 'react';
import { get } from '../../api/client';
import { formatKRW } from '../../utils/format';

interface BalanceData {
  positions: Array<{
    code: string;
    name: string;
    qty: number;
    avg_price: number;
    current_price: number;
    pnl: number;
    pnl_rate: number;
  }>;
  summary: {
    cash: number;
    total_eval: number;
    total_profit_loss: number;
    total_purchase: number;
  };
}

export function AccountSummary() {
  const [balance, setBalance] = useState<BalanceData | null>(null);

  const fetchBalance = useCallback(async () => {
    try {
      const data = await get<BalanceData>('/account/balance');
      setBalance(data);
    } catch {
      // 무시
    }
  }, []);

  useEffect(() => {
    fetchBalance();
    const interval = setInterval(fetchBalance, 30000); // 30초
    return () => clearInterval(interval);
  }, [fetchBalance]);

  if (!balance) {
    return (
      <div className="bg-card rounded-lg border border-border p-3">
        <span className="text-xs text-text-muted">계좌 로딩 중...</span>
      </div>
    );
  }

  const s = balance.summary;
  const totalAsset = s.cash + s.total_eval;
  const halfAlloc = Math.floor(totalAsset / 2);

  return (
    <div className="bg-card rounded-lg border border-border p-3">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-semibold text-text-muted">모의투자 계좌</span>
        <span className="text-xs text-text-muted">5:5 배분</span>
      </div>
      <div className="grid grid-cols-4 gap-3 text-xs">
        <div>
          <span className="text-text-muted">총 자산</span>
          <div className="text-sm font-bold">{formatKRW(totalAsset)}</div>
        </div>
        <div>
          <span className="text-text-muted">예수금</span>
          <div>{formatKRW(s.cash)}</div>
        </div>
        <div>
          <span className="text-text-muted">종목별 배분</span>
          <div>{formatKRW(halfAlloc)}</div>
        </div>
        <div>
          <span className="text-text-muted">평가 손익</span>
          <div className={s.total_profit_loss >= 0 ? 'text-rise' : 'text-fall'}>
            {s.total_profit_loss >= 0 ? '+' : ''}{formatKRW(s.total_profit_loss)}
          </div>
        </div>
      </div>
      {balance.positions.length > 0 && (
        <div className="mt-2 pt-2 border-t border-border">
          {balance.positions.map((p) => (
            <div key={p.code} className="flex justify-between text-xs py-0.5">
              <span>{p.name} {p.qty}주</span>
              <span className={p.pnl >= 0 ? 'text-rise' : 'text-fall'}>
                {p.pnl >= 0 ? '+' : ''}{formatKRW(p.pnl)} ({p.pnl_rate.toFixed(2)}%)
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
