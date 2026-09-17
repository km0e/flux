/**
 * skeleton.tsx — the loading placeholder voice.
 *
 * Static blocks, token-derived (a fg wash reads on EVERY surface — the
 * inset terminal host, elev panels, bg) carrying the codebase's existing
 * gentle opacity pulse — the same animate-pulse the terminal status dot
 * uses. No shimmer sweep: the placeholder promises SHAPE, not progress;
 * it stays quiet. Callers own the geometry (height + width utilities)
 * so a skeleton mirrors whatever it stands in for.
 *
 * Provides: Skeleton
 * Depends: lib/cn.ts
 */
import { cn } from '../../lib/cn';

export function Skeleton({ className }: { className?: string }): React.ReactElement {
  return <div aria-hidden="true" className={cn('animate-pulse rounded-sm bg-fg/10', className)} />;
}
