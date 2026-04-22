import { cn } from '../../utils/cn';

interface SkeletonProps {
  className?: string;
  width?: string | number;
  height?: string | number;
  rounded?: 'sm' | 'md' | 'lg' | 'pill';
}

const ROUND = { sm: 'rounded-sm', md: 'rounded-md', lg: 'rounded-lg', pill: 'rounded-pill' };

export function Skeleton({ className, width, height = '0.75rem', rounded = 'sm' }: SkeletonProps) {
  return (
    <div
      className={cn('skeleton', ROUND[rounded], className)}
      style={{ width, height }}
    />
  );
}
