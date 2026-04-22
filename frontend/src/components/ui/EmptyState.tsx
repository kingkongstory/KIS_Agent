import type { ReactNode } from 'react';
import { cn } from '../../utils/cn';

interface EmptyStateProps {
  icon?: ReactNode;
  title?: ReactNode;
  description?: ReactNode;
  className?: string;
  size?: 'sm' | 'md';
}

export function EmptyState({ icon, title, description, className, size = 'md' }: EmptyStateProps) {
  return (
    <div
      className={cn(
        'flex flex-col items-center justify-center text-center text-text-muted',
        size === 'sm' ? 'py-4 gap-1' : 'py-8 gap-2',
        className,
      )}
    >
      {icon && <div className="opacity-60">{icon}</div>}
      {title && <div className="text-sm font-medium text-text-secondary">{title}</div>}
      {description && <div className="text-xs">{description}</div>}
    </div>
  );
}
