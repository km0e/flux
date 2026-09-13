/**
 * integration-ui.tsx — shared building blocks for the server-integration
 * panels (Providers / MCP servers): one typography scale, one spacing
 * rhythm, one row/field shell. Control styling authority stays in
 * components/ui.tsx — these compose layout and text only.
 *
 * Type scale: section labels 12px uppercase faint · row titles 14px
 * medium · secondary mono (urls/commands) 13px muted · hints 12px
 * faint · badges and actions per ui.tsx. Field labels 13px with hints
 * on their own line beneath. Widths: the settings dialog is
 * w-[min(94vw,720px)].
 *
 * Provides: DialogHint, NoticeBar, SectionLabel, RowShell, RowTitle,
 *           RowSub, EmptyState, FormField, RemoveControl,
 *           useRailSelection, MasterDetail, Rail, RailButton,
 *           NewRailButton, DetailPane
 * Depends: lib/cn.ts, components/ui.tsx (Button/IconButton/TextField/
 *          TextArea — the control styling authority)
 */
import { useEffect, useRef, useState } from 'react';
import { Plus, Trash2 } from 'lucide-react';
import { cn } from '../../lib/cn';
import { Button, IconButton } from '../ui';

/** Introductory explanation under the dialog title. The 14px prose is
 * desktop-tuned — on phones it becomes a wall of text, so mobile drops
 * to 12px. */
export function DialogHint({ children }: { children: React.ReactNode }): React.ReactElement {
  return <p className="text-sm leading-relaxed text-muted max-md:text-xs">{children}</p>;
}

/** A highlighted operational notice (e.g. "restart required") — more
 * visible than a plain hint when the dialog carries a caveat the user
 * must not miss. Mobile drops to 12px like the hint. */
export function NoticeBar({ children }: { children: React.ReactNode }): React.ReactElement {
  return (
    <div className="rounded-md border border-warn/30 bg-warn/10 px-3 py-2 text-sm leading-relaxed text-warn max-md:text-xs">
      {children}
    </div>
  );
}

/** Section label above a list or form section — sentence case, quiet. */
export function SectionLabel({ children }: { children: React.ReactNode }): React.ReactElement {
  return <div className="text-xs font-medium text-faint">{children}</div>;
}

/** One entity row's shell — hover raises the border, actions sit right. */
export function RowShell({ children }: { children: React.ReactNode }): React.ReactElement {
  return (
    <li className="flex flex-col gap-1.5 rounded-lg border border-border bg-inset px-4 py-3 transition-colors hover:border-border-strong">
      {children}
    </li>
  );
}

/** Row primary line — the entity id, 13px medium. */
export function RowTitle({ children }: { children: React.ReactNode }): React.ReactElement {
  return <span className="min-w-0 flex-1 truncate text-base font-medium">{children}</span>;
}

/** Row secondary line — mono 12px (urls, launch commands). */
export function RowSub(props: {
  children: React.ReactNode;
  title?: string;
}): React.ReactElement {
  return (
    <span className="truncate font-mono text-sm text-muted" title={props.title}>
      {props.children}
    </span>
  );
}

/** Empty-list placeholder — centered, quiet, one line. */
export function EmptyState({ children }: { children: React.ReactNode }): React.ReactElement {
  return (
    <div className="flex items-center justify-center rounded-lg border border-dashed border-border px-3 py-10 text-sm text-faint">
      {children}
    </div>
  );
}

/** One labeled form field: a visible 13px label above the control, the
 * hint on its own line between them — an inline hint (label + hint in
 * one span) wraps into a ragged mess when the field is narrow (the
 * skills form's half-width Source field was the motivating case), and
 * stacked lines keep the wrap width honest at every panel width. */
export function FormField(props: {
  label: string;
  hint?: string;
  children: React.ReactNode;
  className?: string;
}): React.ReactElement {
  return (
    <label className={cn('flex min-w-0 flex-col gap-1', props.className)}>
      <span className="text-sm text-muted">{props.label}</span>
      {props.hint && <span className="text-xs text-faint">{props.hint}</span>}
      {props.children}
    </label>
  );
}

/**
 * Two-step remove, shared by every row/preview surface (7 call sites
 * used to hand-roll the identical confirming/error dance): a quiet trash
 * affordance that flips to Confirm/Keep, the inline error parked beneath
 * the control, and a reset on failure so a row never sticks in confirm
 * mode. `onRemove` resolves with the inline error (undefined = success —
 * the fresh list arrives via the broadcast).
 */
export function RemoveControl(props: {
  label: string;
  onRemove: () => Promise<string | undefined>;
}): React.ReactElement {
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!confirming) {
    return (
      <span className="flex flex-col items-end gap-0.5">
        <IconButton
          label={props.label}
          title={props.label}
          className="size-7 hover:text-danger"
          onClick={() => setConfirming(true)}
        >
          <Trash2 size={13} />
        </IconButton>
        {error && <span className="max-w-48 text-2xs break-all text-danger">{error}</span>}
      </span>
    );
  }
  return (
    <span className="flex shrink-0 items-center gap-1">
      <Button
        variant="danger"
        size="sm"
        onClick={() => {
          void props.onRemove().then((err) => {
            if (err) {
              setError(err);
              setConfirming(false);
            }
          });
        }}
      >
        Confirm
      </Button>
      <Button variant="ghost" size="sm" onClick={() => setConfirming(false)}>
        Keep
      </Button>
    </span>
  );
}

/**
 * Selection state for the master-detail rails (Providers / MCP / Skills —
 * the repair contract used to be a three-copy ref dance): a selection key
 * or 'new' (the persistent creation row), repaired against the live list
 * with a fixed priority —
 *
 *   1. a freshly-added entry selects itself once its broadcast lands
 *      (`markPending` at add time),
 *   2. a vanished selection (removed here or elsewhere) falls to the
 *      neighbor of the removal (`noteRemoved`), else to 'new',
 *   3. an untouched panel follows the list (first entry, or the creation
 *      form when empty).
 *
 * `keyOf` must be referentially stable (a module-level function) — it
 * rides the repair effect's dependency list.
 */
export function useRailSelection<T>(
  items: T[],
  keyOf: (item: T) => string,
): {
  /** The selected key, or 'new'. */
  selected: string;
  /** Select an entry or the creation row (marks the panel touched). */
  choose: (key: string) => void;
  /** After a successful add: the new entry self-selects on arrival. */
  markPending: (key: string) => void;
  /** At removal time: the neighbor-fallback anchor for the repair. */
  noteRemoved: (key: string) => void;
} {
  const [selected, setSelected] = useState('new');
  const touchedRef = useRef(false);
  const pendingRef = useRef<string | null>(null);
  const removedIndexRef = useRef<number | null>(null);

  const choose = (next: string) => {
    touchedRef.current = true;
    setSelected(next);
  };
  const markPending = (key: string) => {
    pendingRef.current = key;
  };
  const noteRemoved = (key: string) => {
    removedIndexRef.current = items.findIndex((x) => keyOf(x) === key);
  };

  useEffect(() => {
    if (pendingRef.current) {
      const want = pendingRef.current;
      if (items.some((x) => keyOf(x) === want)) {
        touchedRef.current = true;
        setSelected(want);
        pendingRef.current = null;
      }
      return;
    }
    if (touchedRef.current) {
      if (selected !== 'new' && !items.some((x) => keyOf(x) === selected)) {
        const idx = removedIndexRef.current;
        const next =
          idx !== null && items.length > 0 ? items[Math.min(idx, items.length - 1)] : undefined;
        setSelected(next ? keyOf(next) : 'new');
        removedIndexRef.current = null;
      }
      return;
    }
    if (items.length > 0) {
      if (selected !== keyOf(items[0])) setSelected(keyOf(items[0]));
    } else if (selected !== 'new') {
      setSelected('new');
    }
  }, [items, selected, keyOf]);

  return { selected, choose, markPending, noteRemoved };
}

// ── Master-detail (md+ settings sections) ─────────────────────────────
// The desktop settings layout: a fixed-width selection rail and a fluid
// detail pane, both height-bounded by the dialog — each scrolls
// internally, the page never scrolls. Mobile renders the stacked layout
// instead (JS branch on useIsMobile), so these carry no max-md classes.

/** The two-column shell: list column + detail column. */
export function MasterDetail(props: {
  list: React.ReactNode;
  detail: React.ReactNode;
}): React.ReactElement {
  return (
    <div className="grid min-h-0 flex-1 grid-cols-[minmax(220px,280px)_minmax(0,1fr)] gap-4">
      {props.list}
      {props.detail}
    </div>
  );
}

/** The selection rail: an inset track (the app's segmented-control
 * language) hosting the New row and the entry rows; the track scrolls
 * internally when the list outgrows the dialog. */
export function Rail(props: { label: string; children: React.ReactNode }): React.ReactElement {
  return (
    <div className="flex min-h-0 flex-col rounded-lg bg-inset p-1">
      <ul
        aria-label={props.label}
        className="m-0 flex min-h-0 flex-1 list-none flex-col items-stretch gap-0.5 overflow-y-auto p-0"
      >
        {props.children}
      </ul>
    </div>
  );
}

/** One selectable rail row — raised when selected. Title line plus an
 * optional mono secondary line and an optional trailing badge. */
export function RailButton(props: {
  selected: boolean;
  onClick: () => void;
  ariaLabel: string;
  title: string;
  sub?: string;
  badge?: React.ReactNode;
}): React.ReactElement {
  return (
    <li>
      <button
        type="button"
        aria-current={props.selected || undefined}
        aria-label={props.ariaLabel}
        onClick={props.onClick}
        className={cn(
          'w-full cursor-pointer rounded-sm px-2.5 py-1.5 text-left transition-colors duration-fast',
          props.selected ? 'bg-elev text-fg shadow-sm' : 'text-muted hover:bg-hover hover:text-fg',
        )}
      >
        <span className="flex items-center gap-1.5">
          <span className="min-w-0 flex-1 truncate text-sm font-medium">{props.title}</span>
          {props.badge}
        </span>
        {props.sub && <span className="block truncate font-mono text-xs text-faint">{props.sub}</span>}
      </button>
    </li>
  );
}

/** The persistent "+ New" creation row at the rail's top — selected like
 * any entry; the detail pane then carries the creation form. */
export function NewRailButton(props: {
  label: string;
  selected: boolean;
  onClick: () => void;
}): React.ReactElement {
  return (
    <li>
      <button
        type="button"
        aria-label={props.label}
        aria-current={props.selected || undefined}
        onClick={props.onClick}
        className={cn(
          'flex w-full cursor-pointer items-center gap-1.5 rounded-sm border border-dashed px-2.5 py-1.5 text-left text-sm transition-colors duration-fast',
          props.selected
            ? 'border-accent/60 bg-elev text-fg shadow-sm'
            : 'border-border text-muted hover:border-border-strong hover:text-fg',
        )}
      >
        <Plus size={13} aria-hidden="true" />
        {props.label}
      </button>
    </li>
  );
}

/** The detail column: scrolls internally, never the page. */
export function DetailPane({ children }: { children: React.ReactNode }): React.ReactElement {
  return <div className="flex min-h-0 flex-col gap-4 overflow-y-auto pr-1">{children}</div>;
}
