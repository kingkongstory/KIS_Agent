import { useCallback, useEffect } from 'react';
import { getPrice } from '../api/quotations';
import { useStockStore } from '../stores/stockStore';

/** 현재가 조회 훅 (5초마다 갱신) */
export function useStockPrice() {
  const { selectedCode, setPriceData, setError } = useStockStore();

  const fetchPrice = useCallback(async () => {
    if (!selectedCode) return;
    try {
      const data = await getPrice(selectedCode);
      setPriceData(data);
    } catch (e) {
      setError(e instanceof Error ? e.message : '현재가 조회 실패');
    }
  }, [selectedCode, setPriceData, setError]);

  useEffect(() => {
    fetchPrice();
    const interval = setInterval(fetchPrice, 5000);
    return () => clearInterval(interval);
  }, [fetchPrice]);
}
