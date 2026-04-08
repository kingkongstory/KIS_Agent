import { useState, useEffect } from 'react';
import { getBalance } from '../api/account';
import type { BalanceData } from '../types/account';
import { formatKRW, formatPercent, valueColorClass } from '../utils/format';

export function PortfolioPage() {
  const [balance, setBalance] = useState<BalanceData | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getBalance()
      .then(setBalance)
      .catch((e) => setError(e.message));
  }, []);

  if (error) {
    return <div className="text-fall p-4">오류: {error}</div>;
  }

  if (!balance) {
    return <div className="text-text-muted p-4">로딩 중...</div>;
  }

  return (
    <div className="flex flex-col gap-4">
      {/* 계좌 요약 */}
      <div className="bg-card rounded-lg border border-border p-4">
        <h2 className="text-sm font-semibold text-text-muted mb-3">계좌 요약</h2>
        <div className="grid grid-cols-4 gap-6">
          <div>
            <span className="text-xs text-text-muted">예수금</span>
            <div className="text-lg font-bold">{formatKRW(balance.summary.cash)}</div>
          </div>
          <div>
            <span className="text-xs text-text-muted">총평가</span>
            <div className="text-lg font-bold">{formatKRW(balance.summary.total_eval)}</div>
          </div>
          <div>
            <span className="text-xs text-text-muted">총매입</span>
            <div className="text-lg">{formatKRW(balance.summary.total_purchase)}</div>
          </div>
          <div>
            <span className="text-xs text-text-muted">총손익</span>
            <div className={`text-lg font-bold ${valueColorClass(balance.summary.total_profit_loss)}`}>
              {formatKRW(balance.summary.total_profit_loss)}
            </div>
          </div>
        </div>
      </div>

      {/* 보유 종목 테이블 */}
      <div className="bg-card rounded-lg border border-border overflow-hidden">
        <table className="w-full text-sm">
          <thead className="bg-app-bg text-text-muted text-xs">
            <tr>
              <th className="text-left px-4 py-2">종목</th>
              <th className="text-right px-4 py-2">수량</th>
              <th className="text-right px-4 py-2">평균가</th>
              <th className="text-right px-4 py-2">현재가</th>
              <th className="text-right px-4 py-2">손익</th>
              <th className="text-right px-4 py-2">수익률</th>
            </tr>
          </thead>
          <tbody>
            {balance.positions.map((pos) => (
              <tr key={pos.stock_code} className="border-t border-border hover:bg-border/20">
                <td className="px-4 py-2">
                  <div className="font-medium">{pos.stock_name}</div>
                  <div className="text-xs text-text-muted">{pos.stock_code}</div>
                </td>
                <td className="text-right px-4 py-2">{formatKRW(pos.quantity)}</td>
                <td className="text-right px-4 py-2">{formatKRW(Math.round(pos.avg_price))}</td>
                <td className="text-right px-4 py-2">{formatKRW(pos.current_price)}</td>
                <td className={`text-right px-4 py-2 ${valueColorClass(pos.profit_loss)}`}>
                  {formatKRW(pos.profit_loss)}
                </td>
                <td className={`text-right px-4 py-2 ${valueColorClass(pos.profit_loss_rate)}`}>
                  {formatPercent(pos.profit_loss_rate)}
                </td>
              </tr>
            ))}
            {balance.positions.length === 0 && (
              <tr>
                <td colSpan={6} className="text-center py-8 text-text-muted">
                  보유 종목이 없습니다
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
