import { useEffect, useRef } from 'react';
import { createChart, type IChartApi, type ISeriesApi, type CandlestickData, type Time } from 'lightweight-charts';
import { useStockStore } from '../../stores/stockStore';

export function CandleChart() {
  const containerRef = useRef<HTMLDivElement>(null);
  const chartRef = useRef<IChartApi | null>(null);
  const candleSeriesRef = useRef<ISeriesApi<'Candlestick'> | null>(null);
  const volumeSeriesRef = useRef<ISeriesApi<'Histogram'> | null>(null);
  const { candles, indicators } = useStockStore();

  // 차트 초기화
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
      crosshair: {
        mode: 0,
      },
      rightPriceScale: {
        borderColor: '#2A2A2A',
      },
      timeScale: {
        borderColor: '#2A2A2A',
        timeVisible: false,
      },
    });

    const candleSeries = chart.addCandlestickSeries({
      upColor: '#00D084',
      downColor: '#FF6B35',
      borderUpColor: '#00D084',
      borderDownColor: '#FF6B35',
      wickUpColor: '#00D084',
      wickDownColor: '#FF6B35',
    });

    const volumeSeries = chart.addHistogramSeries({
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

  // 데이터 업데이트
  useEffect(() => {
    if (!candleSeriesRef.current || !volumeSeriesRef.current) return;

    const candleData: CandlestickData<Time>[] = candles.map((c) => ({
      time: c.date as Time,
      open: c.open,
      high: c.high,
      low: c.low,
      close: c.close,
    }));

    const volumeData = candles.map((c) => ({
      time: c.date as Time,
      value: c.volume,
      color: c.close >= c.open ? 'rgba(0, 208, 132, 0.3)' : 'rgba(255, 107, 53, 0.3)',
    }));

    candleSeriesRef.current.setData(candleData);
    volumeSeriesRef.current.setData(volumeData);

    if (candles.length > 0) {
      chartRef.current?.timeScale().fitContent();
    }
  }, [candles]);

  // 지표 오버레이 (라인 시리즈)
  useEffect(() => {
    if (!chartRef.current) return;

    // 간단한 구현: 지표 데이터를 로그로 출력 (실제로는 라인 시리즈 추가)
    // 지표별 라인 시리즈 관리는 별도 리팩토링이 필요
    indicators.forEach((ind) => {
      if (ind.values.length > 0) {
        console.log(`지표 ${ind.name}: ${ind.values.length}개 데이터 포인트`);
      }
    });
  }, [indicators]);

  return (
    <div className="bg-chart-bg rounded-lg border border-border overflow-hidden">
      <div ref={containerRef} className="w-full h-[500px]" />
    </div>
  );
}
