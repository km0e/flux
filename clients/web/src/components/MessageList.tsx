/**
 * MessageList.tsx — Host for imperative chat panes and the scroll button.
 *
 * Per-chat panes are created imperatively by panes.ts / stream.ts inside
 * #messages-wrap; this component renders the host + the floating
 * scroll-to-bottom button for the active chat.
 *
 * Provides: MessageList
 * Depends: core/state.ts, services/panes.ts, lib/dom.ts
 */
import { useEffect } from 'react';
import { ArrowDown } from 'lucide-react';
import { useFlux } from '../core/state';
import { cn } from '../lib/cn';
import { alwaysScrollToBottom, isNearBottom } from '../lib/dom';
import { getPane } from '../services/panes';
import { TranscriptSearchBar } from './TranscriptSearchBar';

export function MessageList(): React.ReactElement {
  // Always-visible affordance: show the jump-to-bottom button whenever we
  // are not at the bottom (not streaming-only — finishing a long reply needs
  // the jump back just as much).
  const visible = useFlux((s) => s.scrollBtnVisible);
  const cid = useFlux((s) => s.activeChatId);
  const live = useFlux((s) => (cid ? (s.streaming[cid] ?? false) : false));

  useEffect(() => {
    // Stream appends change scrollHeight without firing scroll events —
    // recompute at round start AND end (moved out of the store action: a
    // store action must not reach into the DOM). The button is visible
    // whenever the user is away from the bottom, streaming or not.
    const pane = cid
      ? document.querySelector<HTMLElement>(`[data-chat-id="${cid}"]`)
      : null;
    useFlux.setState({ scrollBtnVisible: pane ? !isNearBottom(pane, 80) : false });
  }, [cid, live]);

  const scrollToBottom = () => {
    const cid = useFlux.getState().activeChatId;
    if (!cid) return;
    alwaysScrollToBottom(getPane(cid));
    useFlux.setState({ scrollBtnVisible: false });
  };

  return (
    <div id="messages-wrap" className="relative flex min-h-0 flex-1 flex-col overflow-hidden">
      {/* Per-chat panes managed imperatively in panes.ts / stream.ts */}
      {/* Transcript search (Ctrl/Cmd+F) — floats over the active pane; the
          service paints matches via the Custom Highlight API without ever
          touching the streaming DOM. */}
      <TranscriptSearchBar />
      <button
        id="scroll-bottom-btn"
        aria-label="Scroll to latest message"
        className={cn(
          'absolute right-3 bottom-3 z-10 grid size-9 max-md:size-11 place-items-center rounded-full border border-border',
          'bg-elev text-fg shadow-[var(--fx-shadow-pop)] transition-all duration-base',
          'hover:border-border-strong hover:text-accent',
          visible ? 'translate-y-0 opacity-95' : 'pointer-events-none translate-y-2 opacity-0',
        )}
        onClick={scrollToBottom}
        title="Scroll to bottom"
      >
        <ArrowDown size={15} aria-hidden="true" />
      </button>
    </div>
  );
}
