/**
 * Toasts.tsx — the unified non-blocking notification surface.
 *
 * Filesystem-surface failures (Explorer listings, file reads) report HERE —
 * never inline on the widget that triggered them (a row or a content pane is
 * the wrong place for transport-level errors: they overflow, they repeat per
 * retry, and they read as part of the data). Errors are sticky until
 * dismissed; info auto-dismisses. The stack caps at 4 and dedupes by
 * kind+text (store-side) so a refresh tick hitting the same broken directory
 * refreshes one card instead of stacking spam.
 *
 * Provides: Toasts
 * Depends: core/state.ts, components/ui.tsx
 */
import { useEffect } from 'react';
import { X } from 'lucide-react';
import { useFlux, type ToastEntry } from '../core/state';
import { IconButton } from './ui';
import { cn } from '../lib/cn';

/** Auto-dismiss window for non-error toasts. Errors are sticky. */
const INFO_TTL_MS = 4000;

function ToastItem(props: { toast: ToastEntry }): React.ReactElement {
  const { toast } = props;
  const dismiss = () => useFlux.getState().dismissToast(toast.id);

  useEffect(() => {
    if (toast.kind !== 'info') return;
    const t = setTimeout(dismiss, INFO_TTL_MS);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [toast.id]);

  return (
    <div
      role={toast.kind === 'error' ? 'alert' : 'status'}
      className={cn(
        'pointer-events-auto flex w-[min(92vw,360px)] items-start gap-2 rounded-md border px-3 py-2',
        'shadow-[var(--fx-shadow-pop)] animate-scale-in',
        toast.kind === 'error' ? 'border-danger/40 bg-elev' : 'border-border bg-elev',
      )}
    >
      <span
        aria-hidden="true"
        className={cn(
          'mt-px size-2 shrink-0 rounded-full',
          toast.kind === 'error' ? 'bg-danger' : 'bg-info',
        )}
      />
      <span className="min-w-0 flex-1 text-xs leading-snug break-words text-fg">
        {toast.text}
      </span>
      <IconButton label="Dismiss notification" onClick={dismiss} className="-mr-1 -mt-1 size-5">
        <X size={11} aria-hidden="true" />
      </IconButton>
    </div>
  );
}

export function Toasts(): React.ReactElement | null {
  const toasts = useFlux((s) => s.toasts);
  if (toasts.length === 0) return null;
  return (
    <div
      id="toasts"
      className="pointer-events-none fixed top-12 right-3 z-[60] flex flex-col items-end gap-2"
    >
      {toasts.map((t) => (
        <ToastItem key={t.id} toast={t} />
      ))}
    </div>
  );
}
