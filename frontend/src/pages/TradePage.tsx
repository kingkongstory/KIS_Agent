import { useState } from 'react';
import { placeOrder } from '../api/trading';
import { useStockStore } from '../stores/stockStore';
import { StockPrice } from '../components/stock/StockPrice';
import { useStockPrice } from '../hooks/useStockPrice';
import { formatKRW } from '../utils/format';
import { tickSize } from '../utils/tickSize';
import { cn } from '../utils/cn';
import type { OrderRequest } from '../types/order';

export function TradePage() {
  useStockPrice();
  const { selectedCode, priceData } = useStockStore();

  const [side, setSide] = useState<'buy' | 'sell'>('buy');
  const [orderType, setOrderType] = useState<'limit' | 'market'>('limit');
  const [quantity, setQuantity] = useState('');
  const [price, setPrice] = useState('');
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const currentPrice = priceData?.price ?? 0;
  const tick = tickSize(currentPrice);

  const handleSubmit = async () => {
    setError(null);
    setResult(null);

    const qty = parseInt(quantity);
    if (!qty || qty <= 0) {
      setError('주문 수량을 입력하세요');
      return;
    }

    const order: OrderRequest = {
      stock_code: selectedCode,
      side,
      order_type: orderType,
      quantity: qty,
      price: orderType === 'limit' ? parseInt(price) : undefined,
    };

    setSubmitting(true);
    try {
      const res = await placeOrder(order);
      setResult(`주문 완료 — 주문번호: ${res.order_no}`);
      setQuantity('');
      setPrice('');
    } catch (e) {
      setError(e instanceof Error ? e.message : '주문 실패');
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="flex gap-4">
      <div className="flex-1">
        <StockPrice />
      </div>
      <div className="w-80 bg-card rounded-lg border border-border p-4">
        <h2 className="text-sm font-semibold mb-4">주문</h2>

        {/* 매수/매도 탭 */}
        <div className="flex gap-1 mb-4">
          <button
            onClick={() => setSide('buy')}
            className={cn(
              'flex-1 py-2 rounded text-sm font-medium transition-colors',
              side === 'buy' ? 'bg-rise text-white' : 'bg-border/50 text-text-muted',
            )}
          >
            매수
          </button>
          <button
            onClick={() => setSide('sell')}
            className={cn(
              'flex-1 py-2 rounded text-sm font-medium transition-colors',
              side === 'sell' ? 'bg-fall text-white' : 'bg-border/50 text-text-muted',
            )}
          >
            매도
          </button>
        </div>

        {/* 주문 유형 */}
        <div className="flex gap-2 mb-4">
          {(['limit', 'market'] as const).map((type) => (
            <button
              key={type}
              onClick={() => setOrderType(type)}
              className={cn(
                'px-3 py-1 text-xs rounded',
                orderType === type
                  ? 'bg-accent text-white'
                  : 'bg-border/50 text-text-muted',
              )}
            >
              {type === 'limit' ? '지정가' : '시장가'}
            </button>
          ))}
        </div>

        {/* 가격 입력 */}
        {orderType === 'limit' && (
          <div className="mb-3">
            <label className="text-xs text-text-muted block mb-1">
              가격 (호가단위: {formatKRW(tick)})
            </label>
            <div className="flex gap-1">
              <button
                onClick={() => setPrice(String(Math.max(0, (parseInt(price) || currentPrice) - tick)))}
                className="px-2 bg-border/50 rounded text-text-muted"
              >
                -
              </button>
              <input
                type="number"
                value={price}
                onChange={(e) => setPrice(e.target.value)}
                placeholder={currentPrice.toString()}
                className="flex-1 bg-app-bg border border-border rounded px-2 py-1 text-sm text-text-primary text-right"
              />
              <button
                onClick={() => setPrice(String((parseInt(price) || currentPrice) + tick))}
                className="px-2 bg-border/50 rounded text-text-muted"
              >
                +
              </button>
            </div>
          </div>
        )}

        {/* 수량 입력 */}
        <div className="mb-4">
          <label className="text-xs text-text-muted block mb-1">수량</label>
          <input
            type="number"
            value={quantity}
            onChange={(e) => setQuantity(e.target.value)}
            placeholder="0"
            className="w-full bg-app-bg border border-border rounded px-2 py-1 text-sm text-text-primary text-right"
          />
        </div>

        {/* 에러/결과 */}
        {error && <div className="text-fall text-xs mb-2">{error}</div>}
        {result && <div className="text-rise text-xs mb-2">{result}</div>}

        {/* 주문 버튼 */}
        <button
          onClick={handleSubmit}
          disabled={submitting}
          className={cn(
            'w-full py-2 rounded font-medium text-sm text-white transition-colors',
            side === 'buy' ? 'bg-rise hover:bg-rise/80' : 'bg-fall hover:bg-fall/80',
            submitting && 'opacity-50 cursor-not-allowed',
          )}
        >
          {submitting ? '처리 중...' : side === 'buy' ? '매수 주문' : '매도 주문'}
        </button>
      </div>
    </div>
  );
}
