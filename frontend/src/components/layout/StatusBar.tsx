import { useWsStore } from '../../stores/wsStore';

function getMarketStatusLabel(marketStatus: ReturnType<typeof useWsStore.getState>['marketStatus']): { text: string; color: string } {
  for (const [, op] of marketStatus) {
    if (op.is_trading_halt) return { text: '거래정지', color: 'text-fall' };
    if (op.vi_applied !== '0') {
      const label = op.vi_applied === '1' ? '정적VI' : op.vi_applied === '2' ? '동적VI' : 'VI발동';
      return { text: label, color: 'text-fall' };
    }
  }
  return { text: '', color: '' };
}

export function StatusBar() {
  const connected = useWsStore((s) => s.connected);
  const marketStatus = useWsStore((s) => s.marketStatus);
  const status = getMarketStatusLabel(marketStatus);

  return (
    <footer className="h-6 bg-card border-t border-border flex items-center px-4 text-xs text-text-muted">
      <span className="flex items-center gap-2">
        <span className={`w-2 h-2 rounded-full ${connected ? 'bg-rise' : 'bg-fall'}`} />
        {connected ? '실시간 연결됨' : '연결 끊김'}
      </span>
      {status.text && (
        <span className={`ml-2 font-semibold ${status.color}`}>
          {status.text}
        </span>
      )}
      <span className="ml-auto">v{__APP_VERSION__}</span>
    </footer>
  );
}

declare const __APP_VERSION__: string;
