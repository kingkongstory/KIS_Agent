import { useEffect, useState, useCallback } from 'react';
import { get, post } from '../../api/client';
import { Card, CardHeader, Toggle, Badge, Button } from '../ui';
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
  naver: '네이버',
};

const SOURCE_TONE: Record<SourceKind, 'rise' | 'accent' | 'fall'> = {
  ws: 'rise',
  yahoo: 'accent',
  naver: 'fall',
};

function sourceKind(raw: string | null | undefined): SourceKind | null {
  if (raw === 'ws' || raw === 'yahoo' || raw === 'naver') return raw;
  return null;
}

function RefreshIcon({ spinning }: { spinning?: boolean }) {
  return (
    <svg
      className={cn('w-3.5 h-3.5', spinning && 'animate-spin')}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <polyline points="23 4 23 10 17 10" />
      <polyline points="1 20 1 14 7 14" />
      <path d="M3.51 9a9 9 0 0 1 14.85-3.36L23 10M1 14l4.64 4.36A9 9 0 0 0 20.49 15" />
    </svg>
  );
}

export function StrategyPanel() {
  const [statuses, setStatuses] = useState<StrategyStatus[]>([]);
  const [refreshing, setRefreshing] = useState(false);

  const fetchStatus = useCallback(async () => {
    try {
      const data = await get<StrategyStatus[]>('/strategy/status');
      setStatuses(data);
    } catch { /* 무시 */ }
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
    } catch { /* 무시 */ }
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
    } catch { /* 무시 */ } finally {
      setRefreshing(false);
    }
  };

  const params = statuses[0]?.params;

  return (
    <Card>
      <CardHeader
        title="ORB+FVG 자동매매"
        subtitle="Opening Range × Fair Value Gap"
        action={
          <Button
            variant="ghost"
            size="xs"
            leftIcon={<RefreshIcon spinning={refreshing} />}
            onClick={refreshBackfill}
            disabled={refreshing}
            title="OR 백필 (Yahoo→네이버)"
          >
            {refreshing ? '갱신 중' : 'OR 백필'}
          </Button>
        }
      />

      <div className="grid grid-cols-2 gap-3">
        {statuses.map((s) => {
          const kind = sourceKind(s.or_source);
          return (
            <div
              key={s.code}
              className={cn(
                'rounded-md border p-3 transition-all',
                s.active
                  ? 'border-rise/40 bg-rise-soft/30'
                  : 'border-border bg-surface-2',
              )}
            >
              <div className="flex items-start justify-between gap-2 mb-2">
                <div className="min-w-0">
                  <div className="text-sm font-semibold truncate">{s.name}</div>
                  <div className="text-2xs text-text-muted font-mono">{s.code}</div>
                </div>
                <Toggle
                  checked={s.active}
                  onChange={() => toggle(s.code, s.active)}
                  size="sm"
                  tone="rise"
                  label={`${s.name} 자동매매 ${s.active ? '중단' : '시작'}`}
                />
              </div>

              <div className="flex flex-wrap items-center gap-1.5 mb-2">
                <Badge
                  tone={s.active ? 'rise' : 'muted'}
                  variant="soft"
                  size="xs"
                  dot={s.active}
                >
                  {s.state}
                </Badge>
                {s.today_trades > 0 && (
                  <Badge tone="muted" variant="outline" size="xs">
                    {s.today_trades}건
                  </Badge>
                )}
                {s.today_pnl !== 0 && (
                  <Badge
                    tone={s.today_pnl > 0 ? 'rise' : 'fall'}
                    variant="soft"
                    size="xs"
                  >
                    {s.today_pnl > 0 ? '+' : ''}{s.today_pnl.toFixed(2)}%
                  </Badge>
                )}
              </div>

              {s.message && (
                <div className="text-2xs text-text-muted leading-relaxed mb-2 line-clamp-2">
                  {s.message}
                </div>
              )}

              {s.or_stages && s.or_stages.length > 0 ? (
                <div className="flex flex-wrap gap-1 mb-2">
                  {s.or_stages.map(([stage, h, l]) => (
                    <Badge key={stage} tone="accent" variant="soft" size="xs">
                      <span className="font-semibold mr-1">{stage}</span>
                      <span className="tabular-nums opacity-90">
                        {l.toLocaleString()}~{h.toLocaleString()}
                      </span>
                    </Badge>
                  ))}
                </div>
              ) : s.or_high && s.or_low ? (
                <div className="text-2xs text-text-muted tabular-nums mb-2">
                  OR {s.or_low.toLocaleString()} ~ {s.or_high.toLocaleString()}
                </div>
              ) : null}

              <div className="flex items-center gap-1.5 text-2xs">
                <span className="text-text-disabled uppercase tracking-wider">출처</span>
                {kind ? (
                  <Badge tone={SOURCE_TONE[kind]} variant="soft" size="xs">
                    {SOURCE_LABEL[kind]}
                  </Badge>
                ) : (
                  <Badge tone="muted" variant="outline" size="xs">
                    미수집
                  </Badge>
                )}
              </div>
            </div>
          );
        })}
      </div>

      {params && (
        <div className="mt-3 pt-3 divider-soft flex flex-wrap gap-x-4 gap-y-1 text-xs text-text-muted tabular-nums">
          <span><span className="text-text-disabled">RR</span> 1:{params.rr_ratio}</span>
          <span><span className="text-text-disabled">Trail</span> {params.trailing_r}R</span>
          <span><span className="text-text-disabled">BE</span> {params.breakeven_r}R</span>
          <span><span className="text-text-disabled">Max</span> {params.max_daily_trades}회</span>
          <span><span className="text-text-disabled">Side</span> {params.long_only ? 'Long' : 'L+S'}</span>
        </div>
      )}
    </Card>
  );
}
