import { useNavigate, useLocation } from 'react-router-dom';
import { cn } from '../../utils/cn';

const NAV_ITEMS = [
  { path: '/', icon: '📊', label: '대시보드' },
  { path: '/trade', icon: '📝', label: '주문' },
];

export function Sidebar() {
  const navigate = useNavigate();
  const location = useLocation();

  return (
    <aside className="w-16 bg-card border-r border-border flex flex-col items-center py-4 gap-2">
      <div className="text-2xl mb-4 cursor-pointer" onClick={() => navigate('/')}>
        📈
      </div>
      <div className="w-8 h-px bg-border mb-2" />
      {NAV_ITEMS.map((item) => (
        <button
          key={item.path}
          onClick={() => navigate(item.path)}
          className={cn(
            'w-10 h-10 rounded-lg flex items-center justify-center text-lg transition-colors',
            location.pathname === item.path
              ? 'bg-accent/20 text-accent'
              : 'text-text-muted hover:bg-border/50',
          )}
          title={item.label}
        >
          {item.icon}
        </button>
      ))}
    </aside>
  );
}
