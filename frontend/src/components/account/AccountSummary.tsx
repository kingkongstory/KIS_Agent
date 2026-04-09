import { useEffect, useRef } from 'react';
import { get } from '../../api/client';
import { useWsStore } from '../../stores/wsStore';
import { formatKRW } from '../../utils/format';
import type { BalanceSnapshot } from '../../types/websocket';

const STOCKS = [
  { code: '122630', name: 'KODEX 레버리지' },
  { code: '114800', name: 'KODEX 인버스' },
];

export function AccountSummary() {
  const balance = useWsStore((s) => s.balance);
  const balanceVersion = useWsStore((s) => s.balanceVersion);
  const updateBalance = useWsStore((s) => s.updateBalance);
  const prevVersion = useRef(balanceVersion);

  // 마운트 시 REST로 초기 잔고 로드
  useEffect(() => {
    (async () => {
      try {
        const data = await get<BalanceSnapshot>('/account/balance');
        updateBalance({ ...data, type: 'BalanceSnapshot' });
      } catch { /* 무시 */ }
    })();
  }, [updateBalance]);

  // 주문 체결 시 즉시 잔고 갱신 (1회성 REST fetch)
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
      <div className="bg-card rounded-lg border border-border p-3">
        <span className="text-xs text-text-muted">계좌 로딩 중...</span>
      </div>
    );
  }

  const s = balance.summary;
  const totalAsset = s.total_eval;

  // 종목별 보유 현황
  const posMap = new Map(balance.positions.map((p) => [p.stock_code, p]));

  return (
    <div className="bg-card rounded-lg border border-border p-3">
      <div className="flex items-center justify-between mb-2">
        <span className="text-xs font-semibold text-text-muted">모의투자 계좌</span>
        <span className="text-xs text-text-muted">전액 투입 (선착순)</span>
      </div>
      <div className="grid grid-cols-3 gap-3 text-xs">
        <div>
          <span className="text-text-muted">총 자산</span>
          <div className="text-sm font-bold">{formatKRW(totalAsset)}</div>
        </div>
        <div>
          <span className="text-text-muted">예수금</span>
          <div>{formatKRW(s.cash)}</div>
        </div>
        <div>
          <span className="text-text-muted">평가 손익</span>
          <div className={s.total_profit_loss >= 0 ? 'text-rise' : 'text-fall'}>
            {s.total_profit_loss >= 0 ? '+' : ''}{formatKRW(s.total_profit_loss)}
          </div>
        </div>
      </div>
      {/* 종목별 배분 현황 */}
      <div className="mt-2 pt-2 border-t border-border grid grid-cols-2 gap-2">
        {STOCKS.map(({ code, name }) => {
          const pos = posMap.get(code);
          return (
            <div key={code} className="text-xs">
              <div className="flex items-center justify-between">
                <span className="text-text-muted">{name}</span>
              </div>
              {pos ? (
                <div className="flex items-center justify-between mt-0.5">
                  <span className="text-text-muted">
                    {pos.quantity}주 @ {pos.avg_price.toLocaleString()}
                  </span>
                  <span className={pos.profit_loss >= 0 ? 'text-rise' : 'text-fall'}>
                    {pos.profit_loss >= 0 ? '+' : ''}{formatKRW(pos.profit_loss)}
                    ({pos.profit_loss_rate.toFixed(2)}%)
                  </span>
                </div>
              ) : (
                <div className="text-text-muted mt-0.5">대기</div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
