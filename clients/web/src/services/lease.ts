/**
 * lease.ts — the chat lease handover sequence.
 *
 * Switching the active chat hands over the lease: chat_close the old chat
 * (full exit = unsubscribe + release; the protocol merged the release
 * semantics), then claim the new one. Claim is the single message —
 * history snapshot + subscription + lease arrive together (the server
 * enqueues history before activating the subscription); on failure
 * error{chat_busy} arrives asynchronously and handlers.ts flips the new pane
 * to read-only, sending chat_open (the viewer fallback).
 *
 * Provides: switchLease
 * Depends: core/state.ts, core/bridge.ts
 */
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { clearPaneMessages } from './panes';
import { isPaneStale, markPaneStale } from './stream-handler';

/** Release fromId and claim toId. Either id may be '' (no-op on that side).
 * close = full exit (unsubscribe + release; the protocol merged the release
 * semantics). */
export function switchLease(fromId: string, toId: string): void {
  if (fromId) {
    // Departing while a round is live: the round's end (stream_end) is
    // delivered only to subscribers, so the pane freezes mid-round AND the
    // streaming flag goes stale. Mark the pane — the return path re-pulls
    // history and MUST re-render it (see stream-handler's stalePanes).
    if (useFlux.getState().streaming[fromId]) markPaneStale(fromId);
    bridge.send({ type: 'chat_close', chat_id: fromId });
  }
  if (toId) {
    // Optimistic clear: error{chat_busy} re-marks it when the claim fails.
    useFlux.getState().setReadOnly(toId, false);
    // Optimistically mark as loaded: until chat_history arrives, the chats
    // broadcast's reopen check (loadedChatId !== active) would claim again —
    // on first connect the subscription path and the chats handler each send
    // a claim (server-idempotent but redundant). resetStreamingForReconnect
    // clears this mark on reconnect, so the reopen check works again.
    useFlux.setState({ loadedChatId: toId });
    // A stale pane (departed mid-round) must not flash its outdated DOM
    // while the claim's snapshot is in flight — wipe it to the empty state
    // now; the snapshot re-paints fully. (Peek, not consume: the render's
    // safety-net override still needs the mark.)
    if (isPaneStale(toId)) clearPaneMessages(toId);
    bridge.send({ type: 'chat_claim', chat_id: toId });
  }
  // Badge-suppression window: the row we LEFT still carries a stale
  // `active` flag until the release's chats broadcast lands — rendering it
  // now would flash an In-use badge onto it for a frame (the row is not
  // selected anymore, so the stale flag becomes visible). The next chats
  // frame clears this (setChats); the timeout is the fallback for switches
  // that release nothing (no broadcast follows).
  if (fromId && toId) {
    useFlux.setState({ leaseSwitch: { from: fromId, to: toId } });
    setTimeout(() => {
      const cur = useFlux.getState().leaseSwitch;
      if (cur?.from === fromId && cur?.to === toId) {
        useFlux.setState({ leaseSwitch: null });
      }
    }, 1500);
  }
}
