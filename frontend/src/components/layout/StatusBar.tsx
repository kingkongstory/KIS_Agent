import { useStockStore } from '../../stores/stockStore';

export function StatusBar() {
  const { loading, error } = useStockStore();

  return (
    <footer className="h-6 bg-card border-t border-border flex items-center px-4 text-xs text-text-muted">
      <span className="flex items-center gap-2">
        <span className={`w-2 h-2 rounded-full ${error ? 'bg-fall' : 'bg-rise'}`} />
        {error ? `오류: ${error}` : loading ? '로딩 중...' : '연결됨'}
      </span>
      <span className="ml-auto">v{__APP_VERSION__}</span>
    </footer>
  );
}

declare const __APP_VERSION__: string;
