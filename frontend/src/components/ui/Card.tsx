import type { HTMLAttributes, ReactNode } from 'react';
import { cn } from '../../utils/cn';

type CardTone = 'default' | 'rise' | 'fall' | 'accent' | 'muted';

interface CardProps extends HTMLAttributes<HTMLDivElement> {
  tone?: CardTone;
  padding?: 'none' | 'sm' | 'md' | 'lg';
  interactive?: boolean;
}

const TONE_CLASS: Record<CardTone, string> = {
  default: 'border-border',
  rise: 'border-rise/40 shadow-[0_0_0_1px_rgba(31,203,139,0.08),0_0_24px_rgba(31,203,139,0.06)]',
  fall: 'border-fall/40 shadow-[0_0_0_1px_rgba(255,114,69,0.08),0_0_24px_rgba(255,114,69,0.06)]',
  accent: 'border-accent/40 shadow-[0_0_0_1px_rgba(91,141,239,0.08),0_0_24px_rgba(91,141,239,0.06)]',
  muted: 'border-border-subtle bg-surface-2',
};

const PADDING_CLASS = {
  none: '',
  sm: 'p-3',
  md: 'p-4',
  lg: 'p-5',
};

export function Card({
  tone = 'default',
  padding = 'md',
  interactive = false,
  className,
  children,
  ...rest
}: CardProps) {
  return (
    <div
      className={cn(
        'surface-card',
        TONE_CLASS[tone],
        PADDING_CLASS[padding],
        interactive && 'surface-card-hover transition-colors cursor-pointer',
        className,
      )}
      {...rest}
    >
      {children}
    </div>
  );
}

interface CardHeaderProps {
  title: ReactNode;
  subtitle?: ReactNode;
  action?: ReactNode;
  className?: string;
}

export function CardHeader({ title, subtitle, action, className }: CardHeaderProps) {
  return (
    <div className={cn('flex items-start justify-between gap-3 mb-3', className)}>
      <div className="min-w-0">
        <div className="text-sm font-semibold text-text-primary tracking-tight">{title}</div>
        {subtitle && (
          <div className="text-xs text-text-muted mt-0.5">{subtitle}</div>
        )}
      </div>
      {action && <div className="shrink-0">{action}</div>}
    </div>
  );
}
