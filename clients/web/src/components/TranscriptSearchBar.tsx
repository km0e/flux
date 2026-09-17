/**
 * TranscriptSearchBar.tsx — the floating in-transcript search bar
 * (Ctrl/Cmd+F), rendered inside #messages-wrap over the active pane.
 *
 * The SERVICE (services/transcript-search.ts) owns scanning, highlights,
 * and the mutation observer; this component is the keyboard surface and
 * the match projection. Escape does NOT live here — the app-level Escape
 * chain closes the bar before it may cancel a round (one owner, no
 * double-handling: a local handler would race the window listener).
 *
 * Provides: TranscriptSearchBar
 * Depends: core/state.ts, services/transcript-search.ts, components/ui,
 *          lib/cn.ts
 */
import { useEffect, useRef, useState } from 'react';
import { ChevronDown, ChevronUp, X } from 'lucide-react';
import { useFlux } from '../core/state';
import { IconButton } from './ui';
import { Tooltip } from './ui/tooltip';
import {
  closeSearch,
  nextMatch,
  prevMatch,
  searchOpenedForCid,
  searchQuery,
  setQuery,
} from '../services/transcript-search';
import { cn } from '../lib/cn';

export function TranscriptSearchBar(): React.ReactElement | null {
  const open = useFlux((s) => s.searchOpen);
  const matches = useFlux((s) => s.searchMatches);
  const current = useFlux((s) => s.searchCurrent);
  const cid = useFlux((s) => s.activeChatId);
  const inputRef = useRef<HTMLInputElement>(null);
  const [value, setValue] = useState(searchQuery());

  // Search is scoped to ONE conversation — a chat switch closes the bar.
  // The comparison runs against the SERVICE's opened-for chat (not a
  // mount-time ref): a batched switch+open commit keeps what it opened.
  useEffect(() => {
    if (searchOpenedForCid() !== cid) closeSearch();
  }, [cid]);

  // Find-bar focus contract: opening takes focus (query preselected —
  // type to replace), closing returns it to where the user was (usually
  // the composer, so the next Escape cancels the round as documented).
  const restoreRef = useRef<HTMLElement | null>(null);
  useEffect(() => {
    restoreRef.current = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => restoreRef.current?.focus?.();
  }, []);

  if (!open) return null;
  return (
    <div
      id="transcript-search"
      role="search"
      aria-label="Search in conversation"
      className={cn(
        'absolute top-2 right-3 z-20 flex items-center gap-1 rounded-lg border border-border',
        'bg-elev px-1.5 py-1 shadow-[var(--fx-shadow-pop)] animate-fade-in',
      )}
    >
      <input
        ref={inputRef}
        type="text"
        aria-label="Search in conversation"
        placeholder="Search…"
        spellCheck={false}
        autoComplete="off"
        value={value}
        onChange={(e) => {
          setValue(e.target.value);
          setQuery(e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            if (e.shiftKey) prevMatch();
            else nextMatch();
          }
        }}
        className={cn(
          'h-[var(--fx-control-h-sm)] w-44 max-md:w-32 rounded-sm border-0 bg-inset px-2',
          'text-sm text-fg placeholder:text-faint focus:outline-none',
        )}
      />
      <span
        aria-live="polite"
        className={cn(
          'min-w-[3.25rem] text-center font-mono text-2xs tabular-nums',
          matches > 0 ? 'text-muted' : 'text-faint',
        )}
      >
        {matches > 0 ? `${current + 1}/${matches}` : '0/0'}
      </span>
      <Tooltip content="Previous match (Shift+Enter)" side="bottom">
        <IconButton label="Previous match" onClick={prevMatch}>
          <ChevronUp size={14} aria-hidden="true" />
        </IconButton>
      </Tooltip>
      <Tooltip content="Next match (Enter)" side="bottom">
        <IconButton label="Next match" onClick={nextMatch}>
          <ChevronDown size={14} aria-hidden="true" />
        </IconButton>
      </Tooltip>
      <Tooltip content="Close search (Esc)" side="bottom">
        <IconButton label="Close search" onClick={closeSearch}>
          <X size={14} aria-hidden="true" />
        </IconButton>
      </Tooltip>
    </div>
  );
}
