import { useCallback, useEffect, useMemo, useState } from 'react';
import { get } from '../../api/client';
import { useWsStore } from '../../stores/wsStore';
import { STOCK_CODES } from '../../stores/stockStore';
import { Card, CardHeader, Badge, EmptyState } from '../ui';

interface FvgSummary {
  stock_code: string;
  stage: string;
  side: string;
  b_time: string;
  signal_time: string;
  gap_top: number;
  gap_bottom: number;
  gap_size_pct: number;
  b_body_ratio: number;
  or_breakout_pct: number;
  b_volume: number;
  entry_price: number;
  stop_loss: number;
  take_profit: number;
  state: 'pending' | 'aborted' | 'drift_rejected';
  reason: string | null;
  drift_pct: number | null;
  current_price_at_signal: number | null;
  event_count: number;
  latest_event_at: string;
}

function stateBadge(state: string, reason: string | null) {
  if (state === 'drift_rejected') {
    return <Badge tone="fall" variant="soft" size="xs">drift 이탈</Badge>;
  }
  if (state === 'aborted') {
    const label =
      reason === 'fill_timeout_or_cutoff' ? '미체결'
      : reason === 'preempted' ? '선점양보'
      : reason === 'vi_halted' ? 'VI발동'
      : reason === 'price_fetch_failed' ? '가격조회실패'
      : reason === 'api_error_retry_failed' ? 'API장애'
      : reason || 'aborted';
    return <Badge tone="muted" variant="soft" size="xs">{label}</Badge>;
  }
  return <Badge tone="rise" variant="soft" size="xs" dot>대기중</Badge>;
}

export function FVGPanel() {
  const [fvgs, setFvgs] = useState<FvgSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const prices = useWsStore((s) => s.prices);

  const fetchFvgs = useCallback(async () => {
    try {
      const data = await get<FvgSummary[]>('/strategy/fvgs');
      setFvgs(data);
      setLoading(false);
    } catch { /* 무시 */ }
  }, []);

  useEffect(() => {
    fetchFvgs();
    const interval = setInterval(fetchFvgs, 10000);
    return () => clearInterval(interval);
  }, [fetchFvgs]);

  const stockNames = useMemo(() => {
    const m = new Map<string, string>();
    STOCK_CODES.forEach((meta) => m.set(meta.code, meta.name));
    return m;
  }, []);

  const summary = useMemo(() => {
    const total = fvgs.length;
    const pending = fvgs.filter((f) => f.state === 'pending').length;
    const aborted = fvgs.filter((f) => f.state === 'aborted').length;
    const drift = fvgs.filter((f) => f.state === 'drift_rejected').length;
    return { total, pending, aborted, drift };
  }, [fvgs]);

  return (
    <Card padding="md" className="overflow-hidden">
      <CardHeader
        title="FVG 탐지 현황"
        subtitle="3캔들 갭 패턴 + OR 돌파"
        action={
          <div className="flex items-center gap-1.5">
            <Badge tone="muted" variant="outline" size="xs">{summary.total}개</Badge>
            {summary.pending > 0 && <Badge tone="rise" variant="soft" size="xs">대기 {summary.pending}</Badge>}
            {summary.aborted > 0 && <Badge tone="muted" variant="soft" size="xs">중단 {summary.aborted}</Badge>}
            {summary.drift > 0 && <Badge tone="fall" variant="soft" size="xs">이탈 {summary.drift}</Badge>}
          </div>
        }
      />

      {loading ? (
        <EmptyState size="sm" title="로딩 중..." />
      ) : fvgs.length === 0 ? (
        <EmptyState
          size="sm"
          title="당일 FVG 없음"
          description="장 시작 후 탐지되면 표시됩니다"
        />
      ) : (
        <div className="max-h-72 overflow-y-auto -mx-4 -mb-4">
          <table className="w-full text-xs tabular-nums">
            <thead className="text-2xs text-text-muted uppercase tracking-wider sticky top-0 bg-surface backdrop-blur z-10">
              <tr>
                <th className="text-left font-medium pl-4 py-2">형성</th>
                <th className="text-left font-medium px-2 py-2">종목</th>
                <th className="text-left font-medium px-2 py-2">단계</th>
                <th className="text-right font-medium px-2 py-2">Gap</th>
                <th className="text-right font-medium px-2 py-2">크기</th>
                <th className="text-right font-medium px-2 py-2">돌파</th>
                <th className="text-right font-medium px-2 py-2">진입/SL/TP</th>
                <th className="text-right font-medium px-2 py-2">현재가·drift</th>
                <th className="text-left font-medium px-2 py-2">상태</th>
                <th className="text-right font-medium pr-4 py-2">시도</th>
              </tr>
            </thead>
            <tbody>
              {fvgs.map((f) => {
                const priceSnap = prices.get(f.stock_code);
                const liveCur = priceSnap?.price ?? f.current_price_at_signal ?? null;
                const liveDrift =
                  liveCur != null && f.entry_price > 0
                    ? ((liveCur - f.entry_price) / f.entry_price) * 100
                    : null;
                const displayDrift = f.drift_pct ?? liveDrift;
                return (
                  <tr
                    key={`${f.stock_code}-${f.b_time}`}
                    className="border-t border-border-subtle hover:bg-surface-3/50 transition-colors"
                  >
                    <td className="pl-4 py-1.5 text-text-muted font-mono">{f.b_time.slice(0, 5)}</td>
                    <td className="px-2 py-1.5">
                      {(stockNames.get(f.stock_code) || f.stock_code).slice(0, 8)}
                    </td>
                    <td className="px-2 py-1.5 text-text-muted">{f.stage}</td>
                    <td className="px-2 py-1.5 text-right">
                      {f.gap_bottom.toLocaleString()}~{f.gap_top.toLocaleString()}
                    </td>
                    <td className="px-2 py-1.5 text-right text-text-muted">
                      {f.gap_size_pct.toFixed(2)}%
                    </td>
                    <td className="px-2 py-1.5 text-right text-text-muted">
                      {f.or_breakout_pct.toFixed(2)}%
                    </td>
                    <td className="px-2 py-1.5 text-right text-2xs leading-tight">
                      <div className="font-medium">{f.entry_price.toLocaleString()}</div>
                      <div className="text-text-muted">
                        {f.stop_loss.toLocaleString()} / {f.take_profit.toLocaleString()}
                      </div>
                    </td>
                    <td className="px-2 py-1.5 text-right text-2xs leading-tight">
                      {liveCur != null ? (
                        <>
                          <div>{liveCur.toLocaleString()}</div>
                          {displayDrift != null && (
                            <div
                              className={
                                displayDrift > 0.3
                                  ? 'text-fall'
                                  : displayDrift < -0.3
                                    ? 'text-rise'
                                    : 'text-text-muted'
                              }
                            >
                              {displayDrift > 0 ? '+' : ''}
                              {displayDrift.toFixed(2)}%
                            </div>
                          )}
                        </>
                      ) : (
                        <span className="text-text-disabled">-</span>
                      )}
                    </td>
                    <td className="px-2 py-1.5">{stateBadge(f.state, f.reason)}</td>
                    <td className="pr-4 py-1.5 text-right text-text-muted">{f.event_count}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
