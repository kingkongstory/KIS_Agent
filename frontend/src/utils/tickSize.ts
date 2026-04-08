/** 가격대별 호가 단위 */
export function tickSize(price: number): number {
  if (price < 2_000) return 1;
  if (price < 5_000) return 5;
  if (price < 20_000) return 10;
  if (price < 50_000) return 50;
  if (price < 200_000) return 100;
  if (price < 500_000) return 500;
  return 1_000;
}

/** 가격을 호가 단위에 맞춤 */
export function alignPrice(price: number): number {
  const tick = tickSize(price);
  return Math.floor(price / tick) * tick;
}
