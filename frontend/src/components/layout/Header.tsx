import { StockSearch } from '../stock/StockSearch';

export function Header() {
  return (
    <header className="h-12 bg-card border-b border-border flex items-center px-4 gap-4">
      <h1 className="text-sm font-semibold text-text-muted tracking-wide">KIS Agent</h1>
      <div className="flex-1 max-w-md">
        <StockSearch />
      </div>
    </header>
  );
}
