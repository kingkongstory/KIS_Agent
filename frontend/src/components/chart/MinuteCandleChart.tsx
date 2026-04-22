import { useEffect, useRef, useMemo } from 'react';
import {
  createChart,
  CandlestickSeries,
  HistogramSeries,
  type IChartApi,
  type CandlestickData,
  type UTCTimestamp,
} from 'lightweight-charts';
import { useWsStore, type CandleBar } from '../../stores/wsStore';
import { STOCK_CODES } from '../../stores/stockStore';

interface Props {
  index: number;
  timeframe: Timeframe;
}

type Timeframe = 1 | 5 | 15;

/** "HH:MM" → 분 단위 숫자 */
function timeToMinutes(hhmm: string): number {
  const [h, m] = hhmm.split(':').map(Number);
  return h * 60 + m;
}

/** 분 단위 숫자 → "HH:MM" */
function minutesToTime(mins: number): string {
  const h = Math.floor(mins / 60);
  const m = mins % 60;
  return `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`;
}

/** "HH:MM" → 오늘 날짜 기준 UTCTimestamp (KST를 UTC로 취급하여 차트에 KST 표시) */
function timeToTimestamp(hhmm: string): UTCTimestamp {
  const [h, m] = hhmm.split(':').map(Number);
  const now = new Date();
  const utc = Date.UTC(now.getFullYear(), now.getMonth(), now.getDate(), h, m, 0);
  return (utc / 1000) as UTCTimestamp;
}

/** 1분봉 → N분봉 집계 */
function aggregateCandles(bars: CandleBar[], tf: Timeframe): CandleBar[] {
  if (tf === 1) return bars;

  const sorted = [...bars].sort(
    (a, b) => timeToMinutes(a.time) - timeToMinutes(b.time),
  );

  const grouped = new Map<number, CandleBar>();

  for (const bar of sorted) {
    const mins = timeToMinutes(bar.time);
    // 슬롯 시작 시각: N분 단위로 내림 (09:00 기준)
    const slotStart = Math.floor(mins / tf) * tf;

    const existing = grouped.get(slotStart);
    if (existing) {
      existing.high = Math.max(existing.high, bar.high);
      existing.low = Math.min(existing.low, bar.low);
      existing.close = bar.close;
      existing.volume += bar.volume;
    } else {
      grouped.set(slotStart, {
        time: minutesToTime(slotStart),
        open: bar.open,
        high: bar.high,
        low: bar.low,
        close: bar.close,
        volume: bar.volume,
      });
    }
  }

  return Array.from(grouped.values()).sort(
    (a, b) => timeToMinutes(a.time) - timeToMinutes(b.time),
  );
}

export function MinuteCandleChart({ index, timeframe }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const candleSeriesRef = useRef<any>(null);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const volumeSeriesRef = useRef<any>(null);
  const meta = STOCK_CODES[index];

  const candlesMap = useWsStore((s) => s.candles);
  const currentCandleMap = useWsStore((s) => s.currentCandle);
  const candles = useMemo(() => candlesMap.get(meta.code) ?? [], [candlesMap, meta.code]);
  const currentCandle = currentCandleMap.get(meta.code);

  // 차트 초기화
  useEffect(() => {
    if (!containerRef.current) return;

    const chart = createChart(containerRef.current, {
      autoSize: true,
      layout: {
        background: { color: '#0B0E13' },
        textColor: '#7A828F',
        fontSize: 11,
        fontFamily: 'JetBrains Mono, ui-monospace, Menlo, Consolas, monospace',
      },
      grid: {
        vertLines: { color: '#161A21' },
        horzLines: { color: '#161A21' },
      },
      crosshair: { mode: 1 },
      rightPriceScale: { borderColor: '#20252E' },
      timeScale: {
        borderColor: '#20252E',
        timeVisible: true,
        secondsVisible: false,
      },
    });

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: '#1FCB8B',
      downColor: '#FF7245',
      borderUpColor: '#1FCB8B',
      borderDownColor: '#FF7245',
      wickUpColor: '#1FCB8B',
      wickDownColor: '#FF7245',
    });

    const volumeSeries = chart.addSeries(HistogramSeries, {
      priceFormat: { type: 'volume' },
      priceScaleId: 'volume',
    });

    chart.priceScale('volume').applyOptions({
      scaleMargins: { top: 0.8, bottom: 0 },
    });

    chartRef.current = chart;
    candleSeriesRef.current = candleSeries;
    volumeSeriesRef.current = volumeSeries;

    return () => {
      chart.remove();
    };
  }, []);

  // 데이터 업데이트
  useEffect(() => {
    if (!candleSeriesRef.current || !volumeSeriesRef.current) return;

    // 완성된 캔들 + 진행 중 캔들
    const allBars: CandleBar[] = [...candles];
    if (currentCandle) {
      const idx = allBars.findIndex((c) => c.time === currentCandle.time);
      const bar: CandleBar = {
        time: currentCandle.time,
        open: currentCandle.open,
        high: currentCandle.high,
        low: currentCandle.low,
        close: currentCandle.close,
        volume: currentCandle.volume,
      };
      if (idx >= 0) {
        allBars[idx] = bar;
      } else {
        allBars.push(bar);
      }
    }

    // N분봉으로 집계
    const aggregated = aggregateCandles(allBars, timeframe);

    const candleData: CandlestickData<UTCTimestamp>[] = aggregated
      .map((b) => ({
        time: timeToTimestamp(b.time),
        open: b.open,
        high: b.high,
        low: b.low,
        close: b.close,
      }))
      .sort((a, b) => (a.time as number) - (b.time as number));

    const volumeData = aggregated
      .map((b) => ({
        time: timeToTimestamp(b.time),
        value: b.volume,
        color:
          b.close >= b.open
            ? 'rgba(31, 203, 139, 0.35)'
            : 'rgba(255, 114, 69, 0.35)',
      }))
      .sort((a, b) => (a.time as number) - (b.time as number));

    candleSeriesRef.current.setData(candleData);
    volumeSeriesRef.current.setData(volumeData);
  }, [candles, currentCandle, timeframe]);

  const allBars = [...candles];
  if (currentCandle) allBars.push(currentCandle as CandleBar);
  const aggregated = aggregateCandles(allBars, timeframe);

  return (
    <div className="surface-card overflow-hidden flex flex-col">
      <div className="px-3 py-2 text-xs border-b border-border-subtle flex items-center justify-between bg-surface/60">
        <div className="flex items-center gap-2 min-w-0">
          <span className="font-semibold text-text-primary truncate">{meta.name}</span>
          <span className="text-text-muted font-mono text-2xs">{meta.code}</span>
          <span className="px-1.5 py-0.5 rounded bg-accent-soft text-accent text-2xs font-medium">
            {timeframe}m
          </span>
        </div>
        <span className="text-text-muted text-2xs tabular-nums">{aggregated.length}봉</span>
      </div>
      <div ref={containerRef} className="w-full flex-1 min-h-[300px] bg-chart-bg" />
    </div>
  );
}
