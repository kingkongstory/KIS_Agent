import { useWsStore } from '../../stores/wsStore';
import { cn } from '../../utils/cn';

declare const __APP_VERSION__: string;

function useMarketWarning(): { text: string; tone: 'fall' | 'warn' } | null {
  const marketStatus = useWsStore((s) => s.marketStatus);
  for (const [, op] of marketStatus) {
    if (op.is_trading_halt) return { text: '거래정지', tone: 'fall' };
    if (op.vi_applied !== '0') {
      const label = op.vi_applied === '1' ? '정적VI' : op.vi_applied === '2' ? '동적VI' : 'VI발동';
      return { text: label, tone: 'warn' };
    }
  }
  return null;
}

export function StatusBar() {
  const connected = useWsStore((s) => s.connected);
  const warning = useMarketWarning();

  return (
    <footer className="h-7 bg-card/80 backdrop-blur border-t border-border flex items-center px-4 text-xs text-text-muted gap-3 shrink-0">
      <span className="flex items-center gap-1.5">
        <span
          className={cn(
            'w-1.5 h-1.5 rounded-full',
            connected ? 'bg-rise animate-pulse-rise' : 'bg-fall animate-pulse-fall',
          )}
        />
        <span className={connected ? 'text-text-secondary' : 'text-fall'}>
          {connected ? '실시간 연결됨' : '연결 끊김'}
        </span>
      </span>

      {warning && (
        <>
          <span className="w-px h-3 bg-border" />
          <span className={cn('font-semibold', warning.tone === 'fall' ? 'text-fall' : 'text-warn')}>
            {warning.text}
          </span>
        </>
      )}

      <span className="ml-auto font-mono text-text-disabled">v{__APP_VERSION__}</span>
    </footer>
  );
}
