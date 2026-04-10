import { useEffect, useState, useCallback } from 'react';
import { get, post } from '../../api/client';
import { cn } from '../../utils/cn';

interface StrategyParams {
  rr_ratio: number;
  trailing_r: number;
  breakeven_r: number;
  max_daily_trades: number;
  long_only: boolean;
}

interface StrategyStatus {
  code: string;
  name: string;
  active: boolean;
  state: string;
  today_trades: number;
  today_pnl: number;
  message: string;
  or_high: number | null;
  or_low: number | null;
  or_stages: [string, number, number][];
  or_source: string | null;
  params: StrategyParams;
}

type SourceKind = 'ws' | 'yahoo' | 'naver';

const SOURCE_LABEL: Record<SourceKind, string> = {
  ws: '실시간 WS',
  yahoo: 'Yahoo 5m',
  naver: '네이버 (close-only)',
};

/** 출처별 뱃지 색상 — 그린: 실시간/정확, 블루: 외부 정확 OHLC, 오렌지: 정확도 손실 */
const SOURCE_STYLE: Record<SourceKind, string> = {
  ws: 'bg-rise/20 text-rise border border-rise/30',
  yahoo: 'bg-accent/20 text-accent border border-accent/30',
  naver: 'bg-fall/20 text-fall border border-fall/30',
};

function sourceKind(raw: string | null | undefined): SourceKind | null {
  if (raw === 'ws' || raw === 'yahoo' || raw === 'naver') return raw;
  return null;
}

export function StrategyPanel() {
  const [statuses, setStatuses] = useState<StrategyStatus[]>([]);
  const [refreshing, setRefreshing] = useState(false);

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

  const refreshBackfill = async () => {
    if (refreshing) return;
    setRefreshing(true);
    try {
      await post<{ ok: boolean; message: string; sources: Record<string, string> }>(
        '/strategy/refresh-or-backfill',
        {},
      );
      await fetchStatus();
    } catch {
      // 무시
    } finally {
      setRefreshing(false);
    }
  };

  return (
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-center justify-between mb-3">
        <span className="text-sm font-semibold">ORB+FVG 자동매매</span>
        <button
          onClick={refreshBackfill}
          disabled={refreshing}
          title="OR 백필 데이터 새로 받기 (Yahoo 1순위, 네이버 fallback)"
          className={cn(
            'inline-flex items-center gap-1 px-2 py-1 rounded text-xs transition-colors',
            'border border-border text-text-muted hover:text-text hover:border-accent/50',
            refreshing && 'opacity-50 cursor-not-allowed',
          )}
        >
          <svg
            className={cn('w-3 h-3', refreshing && 'animate-spin')}
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <polyline points="23 4 23 10 17 10" />
            <polyline points="1 20 1 14 7 14" />
            <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
          </svg>
          {refreshing ? '새로고침 중…' : 'OR 백필 새로고침'}
        </button>
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
            {s.or_stages && s.or_stages.length > 0 ? (
              <div className="mt-1 flex flex-wrap gap-1">
                {s.or_stages.map(([stage, h, l]) => (
                  <span key={stage} className="text-xs px-1.5 py-0.5 rounded bg-accent/20 text-accent">
                    {stage} {l.toLocaleString()}~{h.toLocaleString()}
                  </span>
                ))}
              </div>
            ) : s.or_high && s.or_low ? (
              <div className="text-xs text-text-muted mt-1">
                OR {s.or_low.toLocaleString()} ~ {s.or_high.toLocaleString()}
              </div>
            ) : null}
            {(() => {
              const kind = sourceKind(s.or_source);
              return (
                <div className="mt-1.5 flex items-center gap-1.5 text-[10px]">
                  <span className="text-text-muted">출처</span>
                  {kind ? (
                    <span
                      className={cn('px-1.5 py-0.5 rounded font-medium', SOURCE_STYLE[kind])}
                      title={SOURCE_LABEL[kind]}
                    >
                      {SOURCE_LABEL[kind]}
                    </span>
                  ) : (
                    <span className="px-1.5 py-0.5 rounded bg-border text-text-muted">
                      미수집
                    </span>
                  )}
                </div>
              );
            })()}
          </div>
        ))}
      </div>
      {statuses.length > 0 && statuses[0].params && (
        <div className="mt-3 pt-2 border-t border-border flex flex-wrap gap-2 text-xs text-text-muted">
          <span>RR 1:{statuses[0].params.rr_ratio}</span>
          <span>Trail {statuses[0].params.trailing_r}R</span>
          <span>BE {statuses[0].params.breakeven_r}R</span>
          <span>Max {statuses[0].params.max_daily_trades}회</span>
          <span>{statuses[0].params.long_only ? 'Long' : 'L+S'}</span>
        </div>
      )}
    </div>
  );
}
