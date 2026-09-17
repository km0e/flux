/**
 * CommandPalette.tsx — the Ctrl/Cmd+K quick switcher: one input over the
 * command registry (services/commands.ts — chats + application actions).
 *
 * Keyboard-first: the input owns ArrowUp/Down + Enter (roving highlight,
 * wrap-around); click runs; Escape is Radix's own dismissal (the
 * app-level Escape chain already yields to open dialogs). Matches filter
 * case-insensitively over title + keywords; the list is a real listbox
 * (aria-activedescendant on the input, options by id).
 *
 * Provides: CommandPalette
 * Depends: core/state.ts, services/commands.ts, lib/cn.ts
 */
import { useEffect, useMemo, useRef, useState } from 'react';
import * as DialogPrimitive from '@radix-ui/react-dialog';
import { useFlux } from '../core/state';
import { listCommands, type CommandAction } from '../services/commands';
import { cn } from '../lib/cn';

export function CommandPalette(): React.ReactElement {
  const open = useFlux((s) => s.paletteOpen);
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const items: CommandAction[] = useMemo(() => {
    const all = listCommands();
    const q = query.trim().toLowerCase();
    if (!q) return all;
    return all.filter((c) =>
      `${c.title} ${c.keywords ?? ''}`.toLowerCase().includes(q),
    );
  }, [open, query]); // eslint-disable-line react-hooks/exhaustive-deps -- re-snapshot on open: the registry reads live store state

  // A fresh open starts from an EMPTY query — palette queries are
  // one-shot navigation (unlike the find bar's remembered search), and a
  // stale filter would silently hide everything on the next visit.
  useEffect(() => {
    if (open) {
      setQuery('');
      setActive(0);
    }
  }, [open]);

  // Keep the active row visible while arrows move through a long list.
  useEffect(() => {
    document.getElementById(`cmd-opt-${active}`)?.scrollIntoView({ block: 'nearest' });
  }, [active, items.length]);

  const run = (c: CommandAction | undefined) => {
    if (!c) return;
    useFlux.getState().setPaletteOpen(false);
    c.run();
  };

  const onInputKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (items.length) setActive((a) => (a + 1) % items.length);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (items.length) setActive((a) => (a - 1 + items.length) % items.length);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      run(items[active]);
    }
  };

  return (
    <DialogPrimitive.Root
      open={open}
      onOpenChange={(o) => {
        if (!o) useFlux.getState().setPaletteOpen(false);
      }}
    >
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-50 bg-black/55 backdrop-blur-[2px] data-[state=open]:animate-fade-in" />
        <DialogPrimitive.Content
          aria-label="Command palette"
          className={cn(
            'fixed top-[14%] left-1/2 z-50 w-[min(94vw,560px)] -translate-x-1/2',
            'overflow-hidden rounded-lg border border-border bg-elev p-1.5',
            'shadow-[var(--fx-shadow-modal)] data-[state=open]:animate-scale-in focus:outline-none',
          )}
        >
          <DialogPrimitive.Title className="sr-only">Command palette</DialogPrimitive.Title>
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              // A new query reshuffles the list — reset the highlight to the
              // top row. Without this a high roving index survives the
              // shrink and points PAST the last option: the highlight row
              // vanishes (aria-activedescendant dangles) and Enter lands on
              // items[active] === undefined — a no-op keystroke.
              setActive(0);
            }}
            onKeyDown={onInputKeyDown}
            placeholder="Type a command…"
            aria-label="Type a command"
            spellCheck={false}
            autoComplete="off"
            role="combobox"
            aria-expanded
            aria-controls="cmd-list"
            aria-activedescendant={items.length ? `cmd-opt-${active}` : undefined}
            className={cn(
              'h-[var(--fx-control-h)] w-full rounded-sm bg-inset px-3 text-base text-fg',
              'placeholder:text-faint focus:outline-none',
            )}
          />
          <ul
            id="cmd-list"
            role="listbox"
            aria-label="Commands"
            className="mt-1.5 max-h-[46vh] overflow-y-auto"
          >
            {items.map((c, i) => (
              <li
                key={c.id}
                id={`cmd-opt-${i}`}
                role="option"
                aria-selected={i === active}
                onMouseMove={() => setActive(i)}
                onClick={() => run(c)}
                className={cn(
                  'flex cursor-pointer items-baseline gap-3 rounded-sm px-3 py-2 text-sm',
                  i === active ? 'bg-active text-fg' : 'text-fg',
                )}
              >
                <span className="min-w-0 flex-1 truncate">{c.title}</span>
                {c.hint && (
                  <span className="max-w-[45%] truncate font-mono text-2xs text-faint">
                    {c.hint}
                  </span>
                )}
              </li>
            ))}
          </ul>
          {items.length === 0 && (
            <div role="status" className="px-3 py-6 text-center text-sm text-muted">
              No matching commands
            </div>
          )}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  );
}
