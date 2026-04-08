import { useState, useCallback, useRef, useEffect } from 'react';
import { searchStocks } from '../../api/quotations';
import { useStockStore } from '../../stores/stockStore';
import type { StockSearchResult } from '../../types/stock';

export function StockSearch() {
  const [query, setQuery] = useState('');
  const [results, setResults] = useState<StockSearchResult[]>([]);
  const [open, setOpen] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const { setSelectedCode } = useStockStore();

  const handleSearch = useCallback(async (q: string) => {
    setQuery(q);
    if (q.length < 2) {
      setResults([]);
      setOpen(false);
      return;
    }

    // 6자리 숫자면 바로 종목코드로 설정
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
  }, [setSelectedCode]);

  const handleSelect = (code: string) => {
    setSelectedCode(code);
    setQuery('');
    setOpen(false);
  };

  // Ctrl+K 단축키
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
      <input
        ref={inputRef}
        type="text"
        value={query}
        onChange={(e) => handleSearch(e.target.value)}
        placeholder="종목 검색 (Ctrl+K)"
        className="w-full bg-app-bg border border-border rounded px-3 py-1 text-sm text-text-primary placeholder-text-muted focus:outline-none focus:border-accent"
      />
      {open && results.length > 0 && (
        <ul className="absolute top-full left-0 right-0 mt-1 bg-card border border-border rounded shadow-lg z-50 max-h-60 overflow-auto">
          {results.map((r) => (
            <li
              key={r.stock_code}
              onClick={() => handleSelect(r.stock_code)}
              className="px-3 py-2 hover:bg-border/50 cursor-pointer flex justify-between text-sm"
            >
              <span>{r.stock_name}</span>
              <span className="text-text-muted">{r.stock_code}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
