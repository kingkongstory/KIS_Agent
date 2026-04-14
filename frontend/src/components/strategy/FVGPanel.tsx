import { useCallback, useEffect, useMemo, useState } from 'react';
import { get } from '../../api/client';
import { useWsStore } from '../../stores/wsStore';
import { STOCK_CODES } from '../../stores/stockStore';

interface FvgSummary {
  stock_code: string;
  stage: string;
  side: string;
  b_time: string;
  signal_time: string;
  gap_top: number;
  gap_bottom: number;
  gap_size_pct: number; // 이미 %
  b_body_ratio: number;
  or_breakout_pct: number; // 이미 %
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
    return (
      <span className="px-1.5 py-0.5 rounded bg-fall/20 text-fall text-[10px]">drift 이탈</span>
    );
  }
  if (state === 'aborted') {
    const label =
      reason === 'fill_timeout_or_cutoff'
        ? '미체결'
        : reason === 'preempted'
          ? '선점양보'
          : reason === 'vi_halted'
            ? 'VI발동'
            : reason === 'price_fetch_failed'
              ? '가격조회실패'
              : reason === 'api_error_retry_failed'
                ? 'API장애'
                : reason || 'aborted';
    return (
      <span className="px-1.5 py-0.5 rounded bg-text-muted/20 text-text-muted text-[10px]">
        {label}
      </span>
    );
  }
  return (
    <span className="px-1.5 py-0.5 rounded bg-rise/20 text-rise text-[10px]">대기중</span>
  );
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
    } catch {
      // 무시
    }
  }, []);

  useEffect(() => {
    fetchFvgs();
    const interval = setInterval(fetchFvgs, 10000);
    return () => clearInterval(interval);
  }, [fetchFvgs]);

  // 종목별 현재가 맵 (drift 실시간 계산용)
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
    <div className="bg-card rounded-lg border border-border p-4">
      <div className="flex items-center justify-between mb-3">
        <span className="text-sm font-semibold">FVG 탐지 현황</span>
        <div className="flex items-center gap-3 text-xs text-text-muted">
          <span>{summary.total}개</span>
          {summary.pending > 0 && <span className="text-rise">대기 {summary.pending}</span>}
          {summary.aborted > 0 && <span className="text-text-muted">중단 {summary.aborted}</span>}
          {summary.drift > 0 && <span className="text-fall">이탈 {summary.drift}</span>}
        </div>
      </div>
      {loading ? (
        <div className="text-xs text-text-muted text-center py-3">로딩 중...</div>
      ) : fvgs.length === 0 ? (
        <div className="text-xs text-text-muted text-center py-3">
          당일 FVG 없음 (장 시작 후 탐지 시 표시)
        </div>
      ) : (
        <div className="max-h-64 overflow-y-auto">
          <table className="w-full text-xs">
            <thead className="text-text-muted sticky top-0 bg-card">
              <tr>
                <th className="text-left py-1 pr-2">형성</th>
                <th className="text-left py-1 pr-2">종목</th>
                <th className="text-left py-1 pr-1">단계</th>
                <th className="text-right py-1 pr-2">Gap</th>
                <th className="text-right py-1 pr-1">크기</th>
                <th className="text-right py-1 pr-1">돌파</th>
                <th className="text-right py-1 pr-2">진입/SL/TP</th>
                <th className="text-right py-1 pr-2">현재가·drift</th>
                <th className="text-left py-1 pr-1">상태</th>
                <th className="text-right py-1">시도</th>
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
                    className="border-t border-border/30 tabular-nums"
                  >
                    <td className="py-1 pr-2 text-text-muted">{f.b_time.slice(0, 5)}</td>
                    <td className="py-1 pr-2">
                      {(stockNames.get(f.stock_code) || f.stock_code).slice(0, 6)}
                    </td>
                    <td className="py-1 pr-1 text-text-muted">{f.stage}</td>
                    <td className="py-1 pr-2 text-right">
                      {f.gap_bottom.toLocaleString()}~{f.gap_top.toLocaleString()}
                    </td>
                    <td className="py-1 pr-1 text-right text-text-muted">
                      {f.gap_size_pct.toFixed(2)}%
                    </td>
                    <td className="py-1 pr-1 text-right text-text-muted">
                      {f.or_breakout_pct.toFixed(2)}%
                    </td>
                    <td className="py-1 pr-2 text-right text-[11px]">
                      <div>{f.entry_price.toLocaleString()}</div>
                      <div className="text-text-muted">
                        {f.stop_loss.toLocaleString()} / {f.take_profit.toLocaleString()}
                      </div>
                    </td>
                    <td className="py-1 pr-2 text-right">
                      {liveCur != null ? (
                        <>
                          <div>{liveCur.toLocaleString()}</div>
                          {displayDrift != null && (
                            <div
                              className={
                                displayDrift > 0.3
                                  ? 'text-fall text-[11px]'
                                  : displayDrift < -0.3
                                    ? 'text-rise text-[11px]'
                                    : 'text-text-muted text-[11px]'
                              }
                            >
                              {displayDrift > 0 ? '+' : ''}
                              {displayDrift.toFixed(2)}%
                            </div>
                          )}
                        </>
                      ) : (
                        <span className="text-text-muted">-</span>
                      )}
                    </td>
                    <td className="py-1 pr-1">{stateBadge(f.state, f.reason)}</td>
                    <td className="py-1 text-right text-text-muted">{f.event_count}</td>
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
