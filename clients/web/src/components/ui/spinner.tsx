/**
 * spinner.tsx — the running indicator (10px, currentcolor ring). Color
 * follows the host's text token so one component serves every context
 * (top-bar working state, panel rows, buttons).
 *
 * Provides: Spinner
 * Depends: lib/cn.ts
 */
import { cn } from '../../lib/cn';

export function Spinner({ className }: { className?: string }): React.ReactElement {
  return (
    <span
      aria-hidden="true"
      className={cn(
        'inline-block size-3 rounded-full border-[1.5px] border-current border-t-transparent animate-spin',
        className,
      )}
    />
  );
}
