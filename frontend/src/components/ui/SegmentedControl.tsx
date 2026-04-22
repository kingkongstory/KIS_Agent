import { cn } from '../../utils/cn';

interface SegmentOption<T extends string> {
  value: T;
  label: string;
  tone?: 'rise' | 'fall' | 'accent';
}

interface SegmentedControlProps<T extends string> {
  options: SegmentOption<T>[];
  value: T;
  onChange: (next: T) => void;
  size?: 'sm' | 'md';
  fullWidth?: boolean;
  className?: string;
}

export function SegmentedControl<T extends string>({
  options,
  value,
  onChange,
  size = 'sm',
  fullWidth = false,
  className,
}: SegmentedControlProps<T>) {
  const heightClass = size === 'sm' ? 'h-8 text-sm' : 'h-9 text-base';
  return (
    <div
      className={cn(
        'inline-flex items-center bg-surface-2 border border-border rounded-md p-0.5 gap-0.5',
        fullWidth && 'w-full',
        className,
      )}
    >
      {options.map((opt) => {
        const isActive = opt.value === value;
        const activeTone =
          opt.tone === 'rise'
            ? 'bg-rise text-white'
            : opt.tone === 'fall'
              ? 'bg-fall text-white'
              : 'bg-accent text-white';
        return (
          <button
            key={opt.value}
            onClick={() => onChange(opt.value)}
            className={cn(
              'flex-1 px-3 rounded font-medium transition-all',
              heightClass,
              isActive
                ? `${activeTone} shadow-xs`
                : 'text-text-muted hover:text-text-primary hover:bg-surface-3',
            )}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
