import { useState, useCallback, useRef, useEffect } from 'react';
import { searchStocks } from '../../api/quotations';
import { useStockStore } from '../../stores/stockStore';
import type { StockSearchResult } from '../../types/stock';
import { Input } from '../ui';
import { cn } from '../../utils/cn';

function SearchIcon() {
  return (
    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className="w-4 h-4">
      <circle cx="11" cy="11" r="7" />
      <path d="M21 21l-4.35-4.35" strokeLinecap="round" />
    </svg>
  );
}

export function StockSearch() {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<StockSearchResult[]>([]);
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const { setSelectedCode } = useStockStore();

  const handleSearch = useCallback(
    async (q: string) => {
      setQuery(q);
      if (q.length < 2) {
        setResults([]);
        setOpen(false);
        return;
      }
      if (/^\d{6}$/.test(q)) {
        setSelectedCode(q);
        setQuery('');
        setOpen(false);
        return;
      }
      try {
        const data = await searchStocks(q);
        setResults(data);
        setOpen(data.length > 0);
      } catch {
        setResults([]);
      }
    },
    [setSelectedCode],
  );

  const handleSelect = (code: string) => {
    setSelectedCode(code);
    setQuery('');
    setOpen(false);
  };

  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
        e.preventDefault();
        inputRef.current?.focus();
      }
      if (e.key === 'Escape') {
        setOpen(false);
        inputRef.current?.blur();
      }
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, []);

  return (
    <div className="relative">
      <Input
        ref={inputRef}
        value={query}
        onChange={(e) => handleSearch(e.target.value)}
        placeholder="종목 검색"
        leftIcon={<SearchIcon />}
        rightAddon={
          <kbd className="px-1.5 py-0.5 rounded bg-surface-3 border border-border text-2xs font-mono text-text-muted">
            Ctrl+K
          </kbd>
        }
      />
      {open && results.length > 0 && (
        <ul
          className={cn(
            'absolute top-full left-0 right-0 mt-1.5 z-50',
            'surface-card shadow-md p-1 max-h-72 overflow-auto',
          )}
        >
          {results.map((r) => (
            <li
              key={r.stock_code}
              onClick={() => handleSelect(r.stock_code)}
              className="px-2.5 py-2 rounded-sm hover:bg-surface-3 cursor-pointer flex items-center justify-between text-sm transition-colors"
            >
              <span className="text-text-primary">{r.stock_name}</span>
              <span className="text-text-muted font-mono text-xs">{r.stock_code}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
