import { useEffect, useRef } from 'react';
import { createChart, CandlestickSeries, HistogramSeries, type IChartApi, type CandlestickData, type Time } from 'lightweight-charts';
import { useStockStore, STOCK_CODES } from '../../stores/stockStore';

interface Props {
  index: number;
}

export function CandleChart({ index }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const candleSeriesRef = useRef<any>(null);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const volumeSeriesRef = useRef<any>(null);
  const stock = useStockStore((s) => s.stocks[index]);
  const meta = STOCK_CODES[index];

  useEffect(() => {
    if (!containerRef.current) return;

    const chart = createChart(containerRef.current, {
      layout: {
        background: { color: '#1A1A1A' },
        textColor: '#A0A0A0',
      },
      grid: {
        vertLines: { color: '#2A2A2A' },
        horzLines: { color: '#2A2A2A' },
      },
      crosshair: { mode: 1 }, // Magnet 모드 (Normal보다 가벼움)
      rightPriceScale: { borderColor: '#2A2A2A' },
      timeScale: { borderColor: '#2A2A2A', timeVisible: false },
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

    const handleResize = () => {
      if (containerRef.current) {
        chart.applyOptions({
          width: containerRef.current.clientWidth,
          height: containerRef.current.clientHeight,
        });
      }
    };

    window.addEventListener('resize', handleResize);
    handleResize();

    return () => {
      window.removeEventListener('resize', handleResize);
      chart.remove();
    };
  }, []);

  useEffect(() => {
    if (!candleSeriesRef.current || !volumeSeriesRef.current) return;

    const candleData: CandlestickData<Time>[] = stock.candles.map((c) => ({
      time: c.date as Time,
      open: c.open,
      high: c.high,
      low: c.low,
      close: c.close,
    }));

    const volumeData = stock.candles.map((c) => ({
      time: c.date as Time,
      value: c.volume,
      color: c.close >= c.open ? 'rgba(0, 208, 132, 0.3)' : 'rgba(255, 107, 53, 0.3)',
    }));

    candleSeriesRef.current.setData(candleData);
    volumeSeriesRef.current.setData(volumeData);

    if (stock.candles.length > 0) {
      chartRef.current?.timeScale().fitContent();
    }
  }, [stock.candles]);

  return (
    <div className="bg-chart-bg rounded-lg border border-border overflow-hidden flex flex-col">
      <div className="px-3 py-2 text-xs text-text-muted border-b border-border">
        {meta.name} ({meta.code}) 일봉
      </div>
      <div ref={containerRef} className="w-full flex-1 min-h-[300px]" />
    </div>
  );
}
