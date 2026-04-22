import { useState } from 'react';
import { placeOrder } from '../api/trading';
import { useStockStore } from '../stores/stockStore';
import { StockPrice } from '../components/stock/StockPrice';
import { StockSearch } from '../components/stock/StockSearch';
import { useStockPrice } from '../hooks/useStockPrice';
import { formatKRW } from '../utils/format';
import { tickSize } from '../utils/tickSize';
import { Card, CardHeader, Button, Input, SegmentedControl, Badge, IconButton } from '../components/ui';
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
  const qtyNum = parseInt(quantity) || 0;
  const priceNum = orderType === 'limit' ? parseInt(price) || currentPrice : currentPrice;
  const totalAmount = qtyNum * priceNum;

  const adjustPrice = (delta: number) => {
    const base = parseInt(price) || currentPrice;
    setPrice(String(Math.max(0, base + delta)));
  };

  const handleSubmit = async () => {
    setError(null);
    setResult(null);

    if (!qtyNum || qtyNum <= 0) {
      setError('주문 수량을 입력하세요');
      return;
    }

    const order: OrderRequest = {
      stock_code: selectedCode,
      side,
      order_type: orderType,
      quantity: qtyNum,
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
    <div className="flex flex-col gap-4">
      <div className="max-w-md">
        <StockSearch />
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-[1fr_360px] gap-4">
        <div className="min-w-0">
          <StockPrice />
        </div>

        <Card padding="md">
          <CardHeader
            title="주문"
            subtitle={priceData?.name || selectedCode}
            action={
              <Badge tone={side === 'buy' ? 'rise' : 'fall'} variant="soft" size="sm">
                {side === 'buy' ? '매수' : '매도'}
              </Badge>
            }
          />

          <SegmentedControl
            options={[
              { value: 'buy', label: '매수', tone: 'rise' },
              { value: 'sell', label: '매도', tone: 'fall' },
            ]}
            value={side}
            onChange={setSide}
            size="md"
            fullWidth
            className="mb-3"
          />

          <SegmentedControl
            options={[
              { value: 'limit', label: '지정가' },
              { value: 'market', label: '시장가' },
            ]}
            value={orderType}
            onChange={setOrderType}
            size="sm"
            fullWidth
            className="mb-4"
          />

          {orderType === 'limit' && (
            <div className="mb-3">
              <div className="flex items-end justify-between mb-1">
                <label className="text-xs text-text-muted font-medium tracking-wide">가격</label>
                <span className="text-2xs text-text-disabled tabular-nums">
                  호가단위 {formatKRW(tick)}
                </span>
              </div>
              <div className="flex gap-1.5">
                <IconButton
                  label="가격 -"
                  variant="secondary"
                  onClick={() => adjustPrice(-tick)}
                >
                  −
                </IconButton>
                <Input
                  type="number"
                  value={price}
                  onChange={(e) => setPrice(e.target.value)}
                  placeholder={currentPrice.toString()}
                  rightAddon="원"
                />
                <IconButton
                  label="가격 +"
                  variant="secondary"
                  onClick={() => adjustPrice(tick)}
                >
                  +
                </IconButton>
              </div>
            </div>
          )}

          <Input
            label="수량"
            type="number"
            value={quantity}
            onChange={(e) => setQuantity(e.target.value)}
            placeholder="0"
            rightAddon="주"
            className="mb-3"
          />

          {qtyNum > 0 && (
            <div className="flex items-center justify-between text-xs mb-3 px-3 py-2 rounded-md bg-surface-2 border border-border-subtle">
              <span className="text-text-muted">예상 주문금액</span>
              <span className="font-semibold tabular-nums text-text-primary">
                {formatKRW(totalAmount)}원
              </span>
            </div>
          )}

          {error && (
            <div className="text-xs text-fall mb-2 px-3 py-2 rounded-md bg-fall-soft border border-fall/30">
              {error}
            </div>
          )}
          {result && (
            <div className="text-xs text-rise mb-2 px-3 py-2 rounded-md bg-rise-soft border border-rise/30">
              {result}
            </div>
          )}

          <Button
            variant={side === 'buy' ? 'rise' : 'fall'}
            size="lg"
            fullWidth
            loading={submitting}
            onClick={handleSubmit}
          >
            {side === 'buy' ? '매수 주문' : '매도 주문'}
          </Button>
        </Card>
      </div>
    </div>
  );
}
