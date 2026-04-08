/** 숫자를 한국 원화 형식으로 포맷 */
export function formatKRW(value: number): string {
  return new Intl.NumberFormat('ko-KR').format(value);
}

/** 숫자를 간략 표기 (억, 만) */
export function formatShortKRW(value: number): string {
  if (Math.abs(value) >= 1_0000_0000) {
    return `${(value / 1_0000_0000).toFixed(1)}억`;
  }
  if (Math.abs(value) >= 1_0000) {
    return `${(value / 1_0000).toFixed(0)}만`;
  }
  return formatKRW(value);
}

/** 퍼센트 포맷 */
export function formatPercent(value: number): string {
  const sign = value > 0 ? '+' : '';
  return `${sign}${value.toFixed(2)}%`;
}

/** 변화량 포맷 (부호 포함) */
export function formatChange(value: number): string {
  const sign = value > 0 ? '+' : '';
  return `${sign}${formatKRW(value)}`;
}

/** 거래량 포맷 */
export function formatVolume(volume: number): string {
  if (volume >= 1_000_000) {
    return `${(volume / 1_000_000).toFixed(1)}M`;
  }
  if (volume >= 1_000) {
    return `${(volume / 1_000).toFixed(1)}K`;
  }
  return volume.toString();
}

/** 가격변동 부호에 따른 색상 클래스 */
export function priceColorClass(changeSign: string): string {
  switch (changeSign) {
    case '1':
    case '2':
      return 'text-rise';
    case '4':
    case '5':
      return 'text-fall';
    default:
      return 'text-flat';
  }
}

/** 숫자 값에 따른 색상 클래스 */
export function valueColorClass(value: number): string {
  if (value > 0) return 'text-rise';
  if (value < 0) return 'text-fall';
  return 'text-flat';
}
