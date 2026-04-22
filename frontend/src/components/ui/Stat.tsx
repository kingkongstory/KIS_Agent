import type { ReactNode } from 'react';
import { cn } from '../../utils/cn';

type StatTone = 'default' | 'rise' | 'fall' | 'accent' | 'muted';

interface StatProps {
  label: ReactNode;
  value: ReactNode;
  hint?: ReactNode;
  tone?: StatTone;
  align?: 'left' | 'right';
  size?: 'sm' | 'md' | 'lg';
  className?: string;
}

const TONE_VALUE: Record<StatTone, string> = {
  default: 'text-text-primary',
  rise: 'text-rise',
  fall: 'text-fall',
  accent: 'text-accent',
  muted: 'text-text-muted',
};

const SIZE_VALUE = {
  sm: 'text-sm font-semibold',
  md: 'text-md font-semibold',
  lg: 'text-xl font-bold tracking-tight',
};

export function Stat({
  label,
  value,
  hint,
  tone = 'default',
  align = 'left',
  size = 'md',
  className,
}: StatProps) {
  return (
    <div className={cn('flex flex-col gap-0.5', align === 'right' && 'items-end text-right', className)}>
      <span className="text-2xs text-text-muted uppercase tracking-wider font-medium">{label}</span>
      <span className={cn('tabular-nums', SIZE_VALUE[size], TONE_VALUE[tone])}>{value}</span>
      {hint && <span className="text-2xs text-text-muted">{hint}</span>}
    </div>
  );
}
