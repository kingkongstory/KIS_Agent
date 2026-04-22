import { useNavigate, useLocation } from 'react-router-dom';
import { cn } from '../../utils/cn';
import type { ReactNode } from 'react';

interface NavItem {
  path: string;
  label: string;
  icon: ReactNode;
}

const NAV_ITEMS: NavItem[] = [
  {
    path: '/',
    label: '대시보드',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <rect x="3" y="3" width="7" height="9" rx="1.5" />
        <rect x="14" y="3" width="7" height="5" rx="1.5" />
        <rect x="14" y="12" width="7" height="9" rx="1.5" />
        <rect x="3" y="16" width="7" height="5" rx="1.5" />
      </svg>
    ),
  },
  {
    path: '/trade',
    label: '주문',
    icon: (
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
        <path d="M3 17l6-6 4 4 8-9" />
        <path d="M14 6h7v7" />
      </svg>
    ),
  },
];

export function Sidebar() {
  const navigate = useNavigate();
  const location = useLocation();

  return (
    <aside className="w-16 bg-card border-r border-border flex flex-col items-center py-4 gap-1 relative z-10">
      {/* 로고 */}
      <button
        onClick={() => navigate('/')}
        className="w-10 h-10 rounded-lg flex items-center justify-center mb-2 group relative overflow-hidden"
        title="KIS Agent"
      >
        <div className="absolute inset-0 bg-gradient-to-br from-rise/30 via-accent/20 to-fall/20 opacity-80 group-hover:opacity-100 transition-opacity" />
        <span className="relative font-bold text-sm tracking-tight text-text-primary">KIS</span>
      </button>

      <div className="w-8 h-px bg-border mb-2" />

      {NAV_ITEMS.map((item) => {
        const isActive = location.pathname === item.path;
        return (
          <button
            key={item.path}
            onClick={() => navigate(item.path)}
            className={cn(
              'relative w-10 h-10 rounded-lg flex items-center justify-center transition-all group',
              isActive
                ? 'text-accent bg-accent-soft'
                : 'text-text-muted hover:text-text-primary hover:bg-surface-2',
            )}
            title={item.label}
          >
            {/* 좌측 액센트 바 */}
            {isActive && (
              <span className="absolute left-[-12px] top-1.5 bottom-1.5 w-0.5 rounded-full bg-accent" />
            )}
            <span className="w-5 h-5">{item.icon}</span>
          </button>
        );
      })}

      {/* 하단 인디케이터 */}
      <div className="mt-auto flex flex-col items-center gap-2">
        <div className="w-2 h-2 rounded-full bg-rise/80 animate-pulse-rise" title="시스템 정상" />
      </div>
    </aside>
  );
}
