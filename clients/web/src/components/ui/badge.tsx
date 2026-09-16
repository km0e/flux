/**
 * badge.tsx — the inline status pill (tone picks the semantic family;
 * the shape stays a true pill per the radius contract).
 *
 * Provides: Badge, BadgeTone
 * Depends: lib/cn.ts
 */
import { cn } from '../../lib/cn';

export type BadgeTone = 'default' | 'warn' | 'accent';

export function Badge({
  tone = 'default',
  className,
  ...rest
}: React.HTMLAttributes<HTMLSpanElement> & { tone?: BadgeTone }): React.ReactElement {
  return (
    <span
      className={cn(
        'inline-flex items-center rounded-full px-1.5 py-px text-2xs font-medium leading-4 whitespace-nowrap',
        tone === 'default' && 'bg-hover text-muted',
        tone === 'warn' && 'bg-warn/15 text-warn',
        tone === 'accent' && 'bg-accent-dim text-accent',
        className,
      )}
      {...rest}
    />
  );
}
