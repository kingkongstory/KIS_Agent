import { cn } from '../../utils/cn';

interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  size?: 'sm' | 'md';
  tone?: 'rise' | 'accent';
  disabled?: boolean;
  label?: string;
}

export function Toggle({
  checked,
  onChange,
  size = 'md',
  tone = 'rise',
  disabled,
  label,
}: ToggleProps) {
  const dimensions = size === 'sm'
    ? { track: 'w-9 h-5', thumb: 'w-3.5 h-3.5', shift: checked ? 'translate-x-[18px]' : 'translate-x-0.5' }
    : { track: 'w-11 h-6', thumb: 'w-4 h-4', shift: checked ? 'translate-x-[22px]' : 'translate-x-1' };
  const onColor = tone === 'rise' ? 'bg-rise' : 'bg-accent';
  return (
    <button
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        'relative inline-flex items-center rounded-full transition-colors shrink-0',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-app-bg',
        dimensions.track,
        checked ? onColor : 'bg-surface-3 border border-border',
        disabled && 'opacity-50 cursor-not-allowed',
      )}
    >
      <span
        className={cn(
          'inline-block rounded-full bg-white shadow-sm transition-transform',
          dimensions.thumb,
          dimensions.shift,
        )}
      />
    </button>
  );
}
