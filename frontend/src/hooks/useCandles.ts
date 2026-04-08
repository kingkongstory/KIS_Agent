import { useCallback, useEffect } from 'react';
import { getCandles, getIndicators } from '../api/quotations';
import { useStockStore } from '../stores/stockStore';

/** 캔들 데이터 + 지표 조회 훅 */
export function useCandles() {
  const {
    selectedCode,
    chartPeriod,
    activeIndicators,
    setCandles,
    setIndicators,
    setLoading,
    setError,
  } = useStockStore();

  const fetchCandles = useCallback(async () => {
    if (!selectedCode) return;
    setLoading(true);
    try {
      const end = new Date().toISOString().slice(0, 10).replace(/-/g, '');
      const startDate = new Date();
      startDate.setFullYear(startDate.getFullYear() - 1);
      const start = startDate.toISOString().slice(0, 10).replace(/-/g, '');

      const candles = await getCandles(selectedCode, start, end, chartPeriod);
      setCandles(candles);

      if (activeIndicators.length > 0) {
        const indicators = await getIndicators(
          selectedCode,
          activeIndicators,
          start,
          end,
        );
        setIndicators(indicators);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : '데이터 조회 실패');
    } finally {
      setLoading(false);
    }
  }, [selectedCode, chartPeriod, activeIndicators, setCandles, setIndicators, setLoading, setError]);

  useEffect(() => {
    fetchCandles();
  }, [fetchCandles]);

  return { refetch: fetchCandles };
}
