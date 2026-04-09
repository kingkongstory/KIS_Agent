import { useWsStore } from '../../stores/wsStore';

export function StatusBar() {
  const connected = useWsStore((s) => s.connected);

  return (
    <footer className="h-6 bg-card border-t border-border flex items-center px-4 text-xs text-text-muted">
      <span className="flex items-center gap-2">
        <span className={`w-2 h-2 rounded-full ${connected ? 'bg-rise' : 'bg-fall'}`} />
        {connected ? '실시간 연결됨' : '연결 끊김'}
      </span>
      <span className="ml-auto">v{__APP_VERSION__}</span>
    </footer>
  );
}

declare const __APP_VERSION__: string;
