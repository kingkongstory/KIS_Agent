import type { HTMLAttributes, ReactNode } from 'react';
import { cn } from '../../utils/cn';

type BadgeTone = 'neutral' | 'rise' | 'fall' | 'accent' | 'warn' | 'info' | 'muted';
type BadgeVariant = 'soft' | 'solid' | 'outline';

interface BadgeProps extends HTMLAttributes<HTMLSpanElement> {
  tone?: BadgeTone;
  variant?: BadgeVariant;
  size?: 'xs' | 'sm';
  dot?: boolean;
  icon?: ReactNode;
}

const TONE_SOFT: Record<BadgeTone, string> = {
  neutral: 'bg-surface-3 text-text-secondary border border-border',
  rise: 'bg-rise-soft text-rise border border-rise/30',
  fall: 'bg-fall-soft text-fall border border-fall/30',
  accent: 'bg-accent-soft text-accent border border-accent/30',
  warn: 'bg-warn-soft text-warn border border-warn/30',
  info: 'bg-cyan-500/10 text-info border border-info/30',
  muted: 'bg-surface-2 text-text-muted border border-border',
};

const TONE_SOLID: Record<BadgeTone, string> = {
  neutral: 'bg-border text-text-primary',
  rise: 'bg-rise text-white',
  fall: 'bg-fall text-white',
  accent: 'bg-accent text-white',
  warn: 'bg-warn text-black',
  info: 'bg-info text-black',
  muted: 'bg-surface-3 text-text-muted',
};

const TONE_OUTLINE: Record<BadgeTone, string> = {
  neutral: 'border border-border text-text-secondary',
  rise: 'border border-rise/50 text-rise',
  fall: 'border border-fall/50 text-fall',
  accent: 'border border-accent/50 text-accent',
  warn: 'border border-warn/50 text-warn',
  info: 'border border-info/50 text-info',
  muted: 'border border-border text-text-muted',
};

const DOT_TONE: Record<BadgeTone, string> = {
  neutral: 'bg-text-muted',
  rise: 'bg-rise',
  fall: 'bg-fall',
  accent: 'bg-accent',
  warn: 'bg-warn',
  info: 'bg-info',
  muted: 'bg-text-muted',
};

export function Badge({
  tone = 'neutral',
  variant = 'soft',
  size = 'sm',
  dot,
  icon,
  className,
  children,
  ...rest
}: BadgeProps) {
  const toneClass =
    variant === 'solid' ? TONE_SOLID[tone] : variant === 'outline' ? TONE_OUTLINE[tone] : TONE_SOFT[tone];
  return (
    <span
      className={cn(
        'inline-flex items-center gap-1 font-medium rounded-pill whitespace-nowrap leading-none',
        size === 'xs' ? 'px-1.5 py-0.5 text-2xs' : 'px-2 py-0.5 text-xs',
        toneClass,
        className,
      )}
      {...rest}
    >
      {dot && (
        <span
          className={cn(
            'w-1.5 h-1.5 rounded-full',
            DOT_TONE[tone],
            (tone === 'rise' || tone === 'fall') && variant === 'soft' && (
              tone === 'rise' ? 'animate-pulse-rise' : 'animate-pulse-fall'
            ),
          )}
        />
      )}
      {icon && <span className="shrink-0">{icon}</span>}
      {children}
    </span>
  );
}
