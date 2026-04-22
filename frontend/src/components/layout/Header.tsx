import { useEffect, useState } from 'react';
import { useLocation } from 'react-router-dom';
import { Badge } from '../ui';

const PAGE_META: Record<string, { title: string; subtitle: string }> = {
  '/': { title: '대시보드', subtitle: '실시간 자동매매 모니터링' },
  '/trade': { title: '주문', subtitle: '수동 매수/매도' },
};

function useClock() {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const t = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(t);
  }, []);
  return now;
}

function MarketStateBadge() {
  const now = useClock();
  const h = now.getHours();
  const m = now.getMinutes();
  const minutes = h * 60 + m;
  const open = 9 * 60;
  const close = 15 * 60 + 30;
  const isOpen = minutes >= open && minutes <= close;
  return (
    <Badge tone={isOpen ? 'rise' : 'muted'} variant="soft" size="sm" dot>
      {isOpen ? '장중' : '장외'}
    </Badge>
  );
}

export function Header() {
  const location = useLocation();
  const meta = PAGE_META[location.pathname] ?? { title: '', subtitle: '' };
  const now = useClock();
  const time = now.toLocaleTimeString('ko-KR', { hour12: false });
  const date = now.toLocaleDateString('ko-KR', { month: 'short', day: 'numeric', weekday: 'short' });

  return (
    <header className="h-14 bg-card/80 backdrop-blur border-b border-border flex items-center px-5 gap-4 shrink-0">
      <div className="flex items-baseline gap-3 min-w-0">
        <h1 className="text-lg font-bold text-text-primary tracking-tight truncate">{meta.title}</h1>
        <span className="text-xs text-text-muted truncate hidden md:inline">{meta.subtitle}</span>
      </div>

      <div className="ml-auto flex items-center gap-3">
        <MarketStateBadge />
        <div className="hidden sm:flex flex-col items-end leading-tight">
          <span className="text-sm font-mono tabular-nums text-text-secondary">{time}</span>
          <span className="text-2xs text-text-muted">{date}</span>
        </div>
      </div>
    </header>
  );
}
