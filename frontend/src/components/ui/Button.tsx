import type { ButtonHTMLAttributes, ReactNode } from 'react';
import { cn } from '../../utils/cn';

type ButtonVariant = 'primary' | 'secondary' | 'ghost' | 'rise' | 'fall' | 'danger';
type ButtonSize = 'xs' | 'sm' | 'md' | 'lg';

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  loading?: boolean;
  leftIcon?: ReactNode;
  rightIcon?: ReactNode;
  fullWidth?: boolean;
}

const VARIANT_CLASS: Record<ButtonVariant, string> = {
  primary:
    'bg-accent text-white hover:bg-accent-strong active:bg-accent-strong shadow-xs hover:shadow-glow-accent',
  secondary:
    'bg-surface-2 text-text-primary border border-border hover:border-border-strong hover:bg-surface-3',
  ghost:
    'bg-transparent text-text-muted hover:text-text-primary hover:bg-surface-2',
  rise:
    'bg-rise text-white hover:bg-rise-strong shadow-xs hover:shadow-glow-rise',
  fall:
    'bg-fall text-white hover:bg-fall-strong shadow-xs hover:shadow-glow-fall',
  danger:
    'bg-danger text-white hover:bg-danger/90',
};

const SIZE_CLASS: Record<ButtonSize, string> = {
  xs: 'h-6 px-2 text-xs gap-1 rounded-sm',
  sm: 'h-8 px-3 text-sm gap-1.5 rounded-sm',
  md: 'h-9 px-4 text-base gap-2 rounded-md',
  lg: 'h-11 px-5 text-md gap-2 rounded-md font-semibold',
};

export function Button({
  variant = 'secondary',
  size = 'sm',
  loading,
  leftIcon,
  rightIcon,
  fullWidth,
  className,
  disabled,
  children,
  ...rest
}: ButtonProps) {
  const isDisabled = disabled || loading;
  return (
    <button
      className={cn(
        'inline-flex items-center justify-center font-medium transition-all',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-app-bg',
        VARIANT_CLASS[variant],
        SIZE_CLASS[size],
        fullWidth && 'w-full',
        isDisabled && 'opacity-50 cursor-not-allowed pointer-events-none',
        className,
      )}
      disabled={isDisabled}
      {...rest}
    >
      {loading ? (
        <Spinner />
      ) : (
        <>
          {leftIcon && <span className="shrink-0">{leftIcon}</span>}
          <span className="truncate">{children}</span>
          {rightIcon && <span className="shrink-0">{rightIcon}</span>}
        </>
      )}
    </button>
  );
}

function Spinner() {
  return (
    <svg className="animate-spin w-3.5 h-3.5" viewBox="0 0 24 24" fill="none">
      <circle cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" opacity="0.25" />
      <path
        d="M22 12a10 10 0 0 1-10 10"
        stroke="currentColor"
        strokeWidth="3"
        strokeLinecap="round"
      />
    </svg>
  );
}

/* 아이콘 전용 정사각 버튼 */
interface IconButtonProps extends Omit<ButtonProps, 'leftIcon' | 'rightIcon'> {
  label: string;
}
export function IconButton({
  label,
  size = 'sm',
  className,
  children,
  ...rest
}: IconButtonProps) {
  const sizeMap = { xs: 'w-6 h-6', sm: 'w-8 h-8', md: 'w-9 h-9', lg: 'w-11 h-11' };
  return (
    <Button
      size={size}
      aria-label={label}
      title={label}
      className={cn('!p-0', sizeMap[size], className)}
      {...rest}
    >
      {children}
    </Button>
  );
}
