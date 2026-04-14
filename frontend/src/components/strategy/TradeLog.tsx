import { useEffect, useState, useCallback } from 'react';
import { get } from '../../api/client';

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

export function TradeLog() {
  const [trades, setTrades] = useState<Trade[]>([]);

  const fetchTrades = useCallback(async () => {
    try {
      const data = await get<Trade[]>('/strategy/trades');
      setTrades(data);
    } catch {
      // 무시
    }
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
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-center justify-between mb-3">
        <span className="text-sm font-semibold">거래 내역</span>
        <div className="flex items-center gap-3 text-xs">
          <span className="text-text-muted">{trades.length}건</span>
          {wins > 0 && <span className="text-rise">{wins}승</span>}
          {losses > 0 && <span className="text-fall">{losses}패</span>}
          {trades.length > 0 && (
            <span className={totalPnl >= 0 ? 'text-rise font-medium' : 'text-fall font-medium'}>
              {totalPnl >= 0 ? '+' : ''}{totalPnl.toFixed(2)}%
            </span>
          )}
        </div>
      </div>
      {trades.length === 0 ? (
        <div className="text-xs text-text-muted text-center py-3">거래 없음</div>
      ) : (
        <div className="max-h-48 overflow-y-auto">
          <table className="w-full text-xs">
            <thead className="text-text-muted sticky top-0 bg-card">
              <tr>
                <th className="text-left py-1 pr-2">시각</th>
                <th className="text-left py-1 pr-2">종목</th>
                <th className="text-right py-1 pr-2">수량</th>
                <th className="text-right py-1 pr-2">진입</th>
                <th className="text-right py-1 pr-2">매수금액</th>
                <th className="text-right py-1 pr-2">청산</th>
                <th className="text-right py-1 pr-2">매도금액</th>
                <th className="text-left py-1 pr-2">사유</th>
                <th className="text-right py-1">손익</th>
              </tr>
            </thead>
            <tbody>
              {trades.map((t) => {
                const buyAmt = t.entry_price * t.quantity;
                const sellAmt = t.exit_price * t.quantity;
                return (
                <tr key={t.id} className="border-t border-border/30">
                  <td className="py-1 pr-2 text-text-muted">
                    {t.entry_time.slice(11, 16)}
                  </td>
                  <td className="py-1 pr-2">{t.stock_name.slice(0, 8)}</td>
                  <td className="py-1 pr-2 text-right tabular-nums text-text-muted">
                    {t.quantity.toLocaleString()}
                  </td>
                  <td className="py-1 pr-2 text-right tabular-nums">
                    {t.entry_price.toLocaleString()}
                  </td>
                  <td className="py-1 pr-2 text-right tabular-nums text-text-muted">
                    {buyAmt.toLocaleString()}
                  </td>
                  <td className="py-1 pr-2 text-right tabular-nums">
                    {t.exit_price.toLocaleString()}
                  </td>
                  <td className="py-1 pr-2 text-right tabular-nums text-text-muted">
                    {sellAmt.toLocaleString()}
                  </td>
                  <td className="py-1 pr-2">
                    <span
                      className={
                        t.exit_reason === 'TakeProfit'
                          ? 'text-rise'
                          : t.exit_reason === 'StopLoss'
                            ? 'text-fall'
                            : 'text-text-muted'
                      }
                    >
                      {t.exit_reason === 'TakeProfit'
                        ? 'TP'
                        : t.exit_reason === 'StopLoss'
                          ? 'SL'
                          : t.exit_reason}
                    </span>
                  </td>
                  <td
                    className={`py-1 text-right tabular-nums font-medium ${t.pnl_pct >= 0 ? 'text-rise' : 'text-fall'}`}
                  >
                    {t.pnl_pct >= 0 ? '+' : ''}
                    {t.pnl_pct.toFixed(2)}%
                  </td>
                </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
