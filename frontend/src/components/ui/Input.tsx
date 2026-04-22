import { forwardRef, type InputHTMLAttributes, type ReactNode } from 'react';
import { cn } from '../../utils/cn';

interface InputProps extends InputHTMLAttributes<HTMLInputElement> {
  label?: string;
  hint?: ReactNode;
  error?: string;
  leftIcon?: ReactNode;
  rightAddon?: ReactNode;
  fullWidth?: boolean;
  inputClassName?: string;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  {
    label,
    hint,
    error,
    leftIcon,
    rightAddon,
    fullWidth = true,
    className,
    inputClassName,
    type = 'text',
    ...rest
  },
  ref,
) {
  return (
    <div className={cn('flex flex-col gap-1', fullWidth && 'w-full', className)}>
      {label && (
        <label className="text-xs text-text-muted font-medium tracking-wide">{label}</label>
      )}
      <div
        className={cn(
          'flex items-center gap-2 h-9 rounded-md bg-app-bg border border-border px-3',
          'transition-colors focus-within:border-accent focus-within:ring-1 focus-within:ring-accent/40',
          error && 'border-fall focus-within:border-fall focus-within:ring-fall/40',
        )}
      >
        {leftIcon && <span className="text-text-muted shrink-0">{leftIcon}</span>}
        <input
          ref={ref}
          type={type}
          className={cn(
            'flex-1 min-w-0 bg-transparent text-text-primary placeholder:text-text-disabled outline-none',
            type === 'number' && 'text-right tabular-nums',
            inputClassName,
          )}
          {...rest}
        />
        {rightAddon && <span className="text-text-muted text-xs shrink-0">{rightAddon}</span>}
      </div>
      {(hint || error) && (
        <span className={cn('text-xs', error ? 'text-fall' : 'text-text-muted')}>
          {error || hint}
        </span>
      )}
    </div>
  );
});
