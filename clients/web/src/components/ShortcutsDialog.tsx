/**
 * ShortcutsDialog.tsx — the keyboard cheat sheet (? / palette action).
 *
 * Data has TWO sources, each the single truth for its own kind: the
 * REGISTRY carries every action's chord (`shortcut` on CommandAction —
 * the sheet reads it via listShortcutRows, so it can never drift from
 * the palette), and this component carries the composer-level bindings
 * that belong to no action (Enter / Shift+Enter / Escape).
 *
 * Provides: ShortcutsDialog
 * Depends: core/state.ts, services/commands.ts, components/ui/dialog
 */
import { useFlux } from '../core/state';
import { listShortcutRows, type ShortcutRow } from '../services/commands';
import { Dialog, DialogContent, DialogTitle } from './ui/dialog';

const COMPOSER_ROWS: ShortcutRow[] = [
  { keys: 'Enter', label: 'Send the message' },
  { keys: 'Shift+Enter', label: 'New line' },
  { keys: 'Escape', label: 'Close the top surface — else stop the round' },
];

const cnKbd =
  'inline-flex h-5 min-w-5 items-center justify-center rounded-xs border border-border ' +
  'bg-inset px-1.5 font-mono text-2xs text-fg';

function Keys({ combo }: { combo: string }): React.ReactElement {
  // 'Ctrl+B' → two chips; a bare 'Enter' renders as one.
  const parts = combo.split('+');
  return (
    <span className="inline-flex items-center gap-1" aria-label={combo}>
      {parts.map((k, i) => (
        <span key={i} className="inline-flex items-center gap-1">
          {i > 0 && <span aria-hidden="true" className="text-faint">+</span>}
          <kbd className={cnKbd}>{k}</kbd>
        </span>
      ))}
    </span>
  );
}

function ShortcutGroup(props: { label: string; rows: ShortcutRow[] }): React.ReactElement {
  return (
    <section className="flex min-w-0 flex-col gap-1.5">
      <h3 className="text-xs font-medium text-faint">{props.label}</h3>
      <ul className="flex flex-col">
        {props.rows.map((r) => (
          <li
            key={r.keys + r.label}
            className="flex items-center justify-between gap-4 rounded-sm px-2 py-1.5 odd:bg-hover/40"
          >
            <span className="min-w-0 flex-1 text-sm text-fg">{r.label}</span>
            <Keys combo={r.keys} />
          </li>
        ))}
      </ul>
    </section>
  );
}

export function ShortcutsDialog(): React.ReactElement {
  const open = useFlux((s) => s.shortcutsOpen);
  return (
    <Dialog
      open={open}
      onOpenChange={(o) => {
        if (!o) useFlux.getState().setShortcutsOpen(false);
      }}
    >
      <DialogContent className="w-[min(94vw,480px)]">
        <DialogTitle>Keyboard shortcuts</DialogTitle>
        <div className="flex flex-col gap-5">
          <ShortcutGroup label="Application" rows={listShortcutRows()} />
          <ShortcutGroup label="Composer" rows={COMPOSER_ROWS} />
        </div>
      </DialogContent>
    </Dialog>
  );
}
