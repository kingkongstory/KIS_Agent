import { useEffect, useState, useCallback } from 'react';
import { get, post } from '../../api/client';
import { cn } from '../../utils/cn';

interface StrategyStatus {
  code: string;
  name: string;
  active: boolean;
  state: string;
  today_trades: number;
  today_pnl: number;
  message: string;
}

export function StrategyPanel() {
  const [statuses, setStatuses] = useState<StrategyStatus[]>([]);

  const fetchStatus = useCallback(async () => {
    try {
      const data = await get<StrategyStatus[]>('/strategy/status');
      setStatuses(data);
    } catch {
      // 무시
    }
  }, []);

  useEffect(() => {
    fetchStatus();
    const interval = setInterval(fetchStatus, 3000);
    return () => clearInterval(interval);
  }, [fetchStatus]);

  const toggle = async (code: string, active: boolean) => {
    try {
      const endpoint = active ? '/strategy/stop' : '/strategy/start';
      await post<StrategyStatus>(endpoint, { code });
      fetchStatus();
    } catch {
      // 무시
    }
  };

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-center gap-2 mb-3">
        <span className="text-sm font-semibold">ORB+FVG 자동매매</span>
      </div>
      <div className="grid grid-cols-2 gap-3">
        {statuses.map((s) => (
          <div
            key={s.code}
            className={cn(
              'rounded-lg border p-3 transition-colors',
              s.active ? 'border-rise/50 bg-rise/5' : 'border-border bg-card',
            )}
          >
            <div className="flex items-center justify-between mb-2">
              <div>
                <span className="text-sm font-medium">{s.name}</span>
                <span className="text-xs text-text-muted ml-2">{s.code}</span>
              </div>
              <button
                onClick={() => toggle(s.code, s.active)}
                className={cn(
                  'relative inline-flex items-center w-11 h-6 rounded-full transition-colors shrink-0',
                  s.active ? 'bg-rise' : 'bg-border',
                )}
              >
                <span
                  className={cn(
                    'inline-block w-4 h-4 rounded-full bg-white transition-transform',
                    s.active ? 'translate-x-6' : 'translate-x-1',
                  )}
                />
              </button>
            </div>
            <div className="flex items-center gap-2 text-xs">
              <span
                className={cn(
                  'px-2 py-0.5 rounded-full',
                  s.active ? 'bg-rise/20 text-rise' : 'bg-border text-text-muted',
                )}
              >
                {s.state}
              </span>
              {s.today_trades > 0 && (
                <span className="text-text-muted">
                  {s.today_trades}건
                </span>
              )}
              {s.today_pnl !== 0 && (
                <span className={s.today_pnl > 0 ? 'text-rise' : 'text-fall'}>
                  {s.today_pnl > 0 ? '+' : ''}{s.today_pnl.toFixed(2)}%
                </span>
              )}
            </div>
            <div className="text-xs text-text-muted mt-1">{s.message}</div>
          </div>
        ))}
      </div>
    </div>
  );
}
