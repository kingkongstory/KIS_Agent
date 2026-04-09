import { useEffect, useRef } from 'react';
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

  const candles = useWsStore((s) => s.candles.get(meta.code) ?? []);
  const currentCandle = useWsStore((s) => s.currentCandle.get(meta.code));

  // 차트 초기화
  useEffect(() => {
    if (!containerRef.current) return;

    const chart = createChart(containerRef.current, {
      autoSize: true,
      layout: {
        background: { color: '#1A1A1A' },
        textColor: '#A0A0A0',
      },
      grid: {
        vertLines: { color: '#2A2A2A' },
        horzLines: { color: '#2A2A2A' },
      },
      crosshair: { mode: 1 },
      rightPriceScale: { borderColor: '#2A2A2A' },
      timeScale: {
        borderColor: '#2A2A2A',
        timeVisible: true,
        secondsVisible: false,
      },
    });

    const candleSeries = chart.addSeries(CandlestickSeries, {
      upColor: '#00D084',
      downColor: '#FF6B35',
      borderUpColor: '#00D084',
      borderDownColor: '#FF6B35',
      wickUpColor: '#00D084',
      wickDownColor: '#FF6B35',
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
            ? 'rgba(0, 208, 132, 0.3)'
            : 'rgba(255, 107, 53, 0.3)',
      }))
      .sort((a, b) => (a.time as number) - (b.time as number));

    candleSeriesRef.current.setData(candleData);
    volumeSeriesRef.current.setData(volumeData);
  }, [candles, currentCandle, timeframe]);

  const allBars = [...candles];
  if (currentCandle) allBars.push(currentCandle as CandleBar);
  const aggregated = aggregateCandles(allBars, timeframe);

  return (
    <div className="bg-chart-bg rounded-lg border border-border overflow-hidden flex flex-col">
      <div className="px-3 py-1.5 text-xs border-b border-border flex items-center justify-between">
        <span className="text-text-muted">
          {meta.name} ({meta.code}) {timeframe}분봉
        </span>
        <span className="text-accent">{aggregated.length}봉</span>
      </div>
      <div ref={containerRef} className="w-full flex-1 min-h-[300px]" />
    </div>
  );
}
