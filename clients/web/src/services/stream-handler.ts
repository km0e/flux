/**
 * stream-handler.ts — Streaming event handlers → imperative DOM operations.
 *
 * Receives text/reasoning/tool delta events and updates the DOM accordingly.
 * Uses StreamController (stream.ts) for live streaming and dom.ts builders
 * for message construction. History rendering lives in history.ts.
 *
 * Provides: handleTextDelta, handleReasoningDelta, handleToolStart,
 *           handleToolResult, handleStreamError, handleStreamGap,
 *           handleStreamEnd, handleStreamCancelled, appendUserMessage,
 *           markInterrupt, discardInterrupt, resetStreamingForReconnect
 * Depends: core/state.ts, services/panes.ts, services/stream.ts, lib/dom.ts
 * Note: DO NOT delete — mount.tsx and ChatView.tsx depend on these exports.
 */

import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { ensurePane, getPaneIfExists } from './panes';
import { getController, disposeController } from './stream';
import { recordToolStart, resetRound, clearAllRounds } from './artifacts';
import {
  createMessageBubble,
  createNoticeBubble,
  createTypingIndicator,
  escapeCssSelector,
  forceFollow,
  markToolCardComplete,
  scrollPaneToBottom,
} from '../lib/dom';

// ── ARIA live announcements ──

let announcerEl: HTMLDivElement | null = null;

function getAnnouncer(): HTMLDivElement {
  if (!announcerEl) {
    announcerEl = document.createElement('div');
    announcerEl.id = 'stream-announcer';
    announcerEl.setAttribute('aria-live', 'polite');
    announcerEl.setAttribute('aria-atomic', 'false');
    announcerEl.className = 'sr-only';
  }
  // Re-attach if the container was recreated (DOM rebuild) — a detached
  // cached element would silently swallow all announcements.
  if (!announcerEl.isConnected) {
    const wrap = document.getElementById('messages-wrap');
    if (wrap) wrap.appendChild(announcerEl);
  }
  return announcerEl;
}

// ── Incremental announcement (P0 — kills the O(n²) full-buffer rescans) ──

/**
 * Trailing unterminated text held across deltas until it completes a sentence.
 * Bounded below — a pathological run-on without punctuation degrades to no
 * announcements instead of growing the scanned window unboundedly.
 */
let announcePending = '';
/** Upper bound; a sentence longer than this is not meaningfully announced. */
const ANNOUNCE_MAX = 512;

/**
 * Announce sentences as they complete, scanning only the newly-appended delta
 * (plus a bounded trailing partial) instead of the whole accumulated buffer
 * on every delta (whole-buffer rescans are O(n²) per delta).
 */
export function incrementallyAnnounce(delta: string): void {
  announcePending += delta;
  if (announcePending.length > ANNOUNCE_MAX) {
    announcePending = announcePending.slice(-ANNOUNCE_MAX);
  }
  const sentences = announcePending.match(/[^。.!！?\n]+[。.!！?\n]/g);
  if (!sentences || sentences.length === 0) return; // nothing complete yet
  const last = sentences[sentences.length - 1];
  const region = getAnnouncer();
  region.textContent = last.trim();
  // Retain only the text after the announced sentence (a new partial, if any).
  const end = announcePending.lastIndexOf(last) + last.length;
  announcePending = announcePending.slice(end);
}

/** Test-only: clear the incremental accumulator so `it` blocks are isolated. */
export function _resetAnnounceForTest(): void {
  announcePending = '';
}

/** Drop the held partial sentence — a finished/interrupted round must not
 * leak its trailing fragment into the next round's announcements. */
function resetAnnounce(): void {
  announcePending = '';
}

// ── Stream handlers (delegate to StreamController) ──

export function handleTextDelta(chatId: string, delta: string): void {
  getController(chatId).appendText(delta);
  // Announce as sentences complete — scan only the delta, not the whole
  // accumulated buffer (P0 kills the O(n²) full-buffer rescans).
  incrementallyAnnounce(delta);
}

export function handleReasoningDelta(chatId: string, delta: string): void {
  getController(chatId).appendReasoning(delta);
}

export function handleToolStart(chatId: string, id: string, name: string, args: string): void {
  getController(chatId).addToolCard(id, name, args);
  // The round's artifact list folds the same event (F-11) — files/invocations
  // since the chat's last user message.
  recordToolStart(chatId, id, name, args);
}

/** tool_preview — the model is still forming a tool call. The identity
 * event creates a pending card; argument fragments append to its live
 * tail. The real tool_start upgrades the card in place; a preview with no
 * tool_start by round end is voided (see voidPendingPreviews). */
export function handleToolPreview(
  chatId: string,
  id: string,
  name?: string,
  argsDelta?: string,
): void {
  const ctrl = getController(chatId);
  if (name !== undefined) ctrl.addToolPreview(id, name);
  if (argsDelta !== undefined) ctrl.appendToolPreviewArgs(id, argsDelta);
}

/** Round wrap-up: drop any still-pending preview cards — they never got
 * their tool_start (cancel / truncation / malformed args), so they must
 * not linger as zombies. Upgraded cards (running/done) are untouched. */
export function voidPendingPreviews(chatId: string): void {
  getPaneIfExists(chatId)
    ?.querySelectorAll('.tool.pending[data-tool-call-id]')
    .forEach((el) => el.remove());
}

export function handleToolResult(chatId: string, id: string, result: string): void {
  // Tool cards live inside the chat's pane. Query pane-scoped so a card in
  // one chat can never be completed by another chat's result, and never
  // create a pane for a chat that has none (a late result after delete).
  const pane = getPaneIfExists(chatId);
  if (!pane) return;
  // The id is model-generated — escape it so a quote/backslash cannot throw
  // SyntaxError and silently drop the result.
  const el = pane.querySelector(
    `[data-tool-call-id="${escapeCssSelector(id)}"]`,
  ) as HTMLElement | null;
  if (!el) return;

  markToolCardComplete(el, result);

  // Scroll only the visible pane (the active chat) — completing a card in a
  // hidden chat must not move the scroll position a user will return to.
  if (useFlux.getState().activeChatId === chatId) {
    scrollPaneToBottom(pane, 50);
  }
}

export function handleStreamError(chatId: string, message: string): void {
  voidPendingPreviews(chatId);
  const ctrl = getController(chatId);
  ctrl.appendError(message);
  ctrl.flushRender();
  useFlux.getState().clearStreaming(chatId);
  clearInterrupt();
  resetAnnounce();
}

/** stream_gap — a slow viewer dropped frames. The transcript is already
 * persisted; re-running chat_open re-pulls history and rebuilds the
 * subscription (the resync). Presented as a clickable notice, not an
 * error state. */
export function handleStreamGap(chatId: string): void {
  const pane = ensurePane(chatId);
  const el = createNoticeBubble('Live updates paused — click to reload');
  el.classList.add('stream-gap');
  el.addEventListener('click', () => {
    // Resubscribe: chat_close first, then reopen. An already-subscribed
    // viewer early-returns in the server's ensure_viewer, so chat_open alone
    // never triggers ViewerGone — the dropped-frame mark stays and the stream
    // freezes; close clears the mark (ViewerGone) and the reopen rebuilds the
    // subscription (the verified resubscription path). Sending on the same
    // channel in order keeps the ordering.
    // Reopen with chat_claim (non-readonly = we believe we hold the lease) —
    // open would silently downgrade the lease holder to a viewer; a refused
    // claim (chat_busy) is degraded by handlers with a follow-up open. A pure
    // viewer (already read-only) still goes through open.
    bridge.send({ type: 'chat_close', chat_id: chatId });
    if (useFlux.getState().readonlyChats[chatId]) {
      bridge.send({ type: 'chat_open', chat_id: chatId });
    } else {
      bridge.send({ type: 'chat_claim', chat_id: chatId });
    }
    el.remove();
  });
  pane.appendChild(el);
  scrollPaneToBottom(pane, 50);
}

// ── stale-pane marks (departure-mid-round bookkeeping) ──

/** Chats whose pane DOM went stale because the session unsubscribed while a
 * round was live (switchLease's chat_close): stream_end for that round is
 * delivered only to subscribers, so the client's streaming flag goes stale
 * AND the pane misses everything the round persisted after departure. The
 * claim's history snapshot MUST re-render such a pane (the safety net's
 * streaming skip would freeze the outdated content — the reported
 * "renders a few messages, gets interrupted, never fully loads"), and the
 * stale DOM must not flash while the snapshot is in flight. */
const stalePanes = new Set<string>();

/** Departure time: the pane's DOM predates the unsubscribe window. */
export function markPaneStale(chatId: string): void {
  stalePanes.add(chatId);
}

/** Peek — the switch path clears the stale DOM without consuming the mark
 * (the claim's history render still needs it to override the skip). */
export function isPaneStale(chatId: string): boolean {
  return stalePanes.has(chatId);
}

/** Consume — the claim's history render: true means the pane is stale even
 * if the streaming flag says otherwise; the safety net must not skip. */
export function takePaneStale(chatId: string): boolean {
  const stale = stalePanes.has(chatId);
  stalePanes.delete(chatId);
  return stale;
}

/** Test-only: the marks are module-level state (file-isolated in vitest). */
export function _resetStalePanesForTest(): void {
  stalePanes.clear();
}

// ── R1 interrupt-send (fused cancel + next message) ──

/**
 * The interrupt-send sequence's client-side bookkeeping. The SERVER now
 * owns the whole interject: one SendMessage{interrupt} fuses "cancel the
 * live round" with "queue my message" on the kernel FIFO (R1), so no
 * park/poll/flush machinery is needed — the bubble appends at send time
 * and the replacement round starts server-side at the wrap-up.
 *
 * What remains client-side is presentation only:
 * - the cancel inside the fused send is SELF-triggered — its
 *   `stream_cancelled` must not show the 'Cancelled' notice (it would
 *   read as an error right after the user's own bubble) and must keep
 *   streaming live through the wrap-up gap (the replacement round follows
 *   in the same event burst; clearing here would flicker Stop→Send);
 * - the wrap-up's `stream_end` keeps streaming ONLY when that cancel was
 *   seen — an interrupt-send on an Idle engine produces no cancel event,
 *   so its stream_end is the replacement round's own end and clears
 *   normally.
 *
 * Single slot — a newer interrupt-send supersedes (the server's kernel
 * queue discipline matches: a cancel kills the older queued turn).
 */
let interruptedChatId: string | null = null;
let interruptedCancelSeen = false;

/** Mark the chat's wrap-up sequence as self-triggered (called at
 * interrupt-send time, before the round's cancel confirmation arrives). */
export function markInterrupt(chatId: string): void {
  interruptedChatId = chatId;
  interruptedCancelSeen = false;
}

/** Drop the interrupt bookkeeping for a chat: an explicit Stop/Escape
 * AFTER an interrupt-send means "stop means stop" — the queued turn dies
 * with the server-side cancel, and the following `stream_cancelled` must
 * behave like a plain user cancel (notice + clear). */
export function discardInterrupt(chatId: string): void {
  if (interruptedChatId === chatId) clearInterrupt();
}

function clearInterrupt(): void {
  interruptedChatId = null;
  interruptedCancelSeen = false;
}

/**
 * The user cancelled the active round — a neutral notice, not an error.
 * The round still wraps up with `stream_end` (idempotent on the same
 * controller), so clearing streaming here is safe.
 *
 * A self-triggered interrupt-send cancel skips the notice — it would read
 * as an error right after their own bubble — and keeps streaming live
 * through the wrap-up gap: the replacement round follows server-side, and
 * `handleStreamEnd` carries the flag through.
 */
export function handleStreamCancelled(chatId: string): void {
  voidPendingPreviews(chatId);
  const ctrl = getController(chatId);
  const selfCancelled = interruptedChatId === chatId;
  if (selfCancelled) interruptedCancelSeen = true;
  if (!selfCancelled) ctrl.appendNotice('Cancelled');
  ctrl.flushRender();
  if (!selfCancelled) useFlux.getState().clearStreaming(chatId);
  resetAnnounce();
}

/** Assistant reply fully streamed and persisted — finalize the DOM and
 * clear streaming state — unless this end belongs to the interrupted
 * sequence's CANCELLED round (a cancel was seen): the replacement round
 * follows in the same event burst, so streaming stays live and the flag
 * retires here (the replacement's own end clears normally). An
 * interrupt-send on an Idle engine never saw a cancel, so its first
 * stream_end IS the replacement round's end.
 * A `finish_reason` of "length"/"content_filter" means the reply was cut
 * short — surface a neutral notice instead of showing it as complete. */
export function handleStreamEnd(chatId: string, finishReason?: string): void {
  voidPendingPreviews(chatId);
  const wrapOnly = interruptedChatId === chatId && interruptedCancelSeen;
  const ctrl = getController(chatId);
  ctrl.flushRender();
  if (finishReason === 'length') {
    ctrl.appendNotice('Reply truncated (context limit reached)');
  } else if (finishReason === 'content_filter') {
    ctrl.appendNotice('Reply truncated by content filter');
  }
  if (!wrapOnly) useFlux.getState().clearStreaming(chatId);
  if (interruptedChatId === chatId) clearInterrupt();
  resetAnnounce();
}

export function appendUserMessage(text: string): void {
  const cid = useFlux.getState().activeChatId;
  if (!cid) return;

  // Stop and dispose any active stream for this chat. dispose() clears
  // streaming as part of its teardown — the optimistic set below must
  // come after it, or the round-level truth is wiped in the same tick.
  disposeController(cid);

  // Round-level truth: a message send starts a new round — the chat is
  // streaming from here until stream_end or an error (error{...}) clears it.
  // Covers reasoning, TTFT, tool execution, and question waits uniformly, so
  // Stop/Escape stay live for the whole round.
  useFlux.getState().setStreaming(cid, true);
  resetRound(cid);
  appendUserBubble(cid, text);
}

/** R1 interrupt-send's user bubble: the same visuals as a plain send, but
 * the live controller is NOT disposed — the round being cancelled still
 * owns it, and disposing mid-round would make its trailing deltas open a
 * second assistant bubble BELOW the user's message. The replacement round
 * opens a fresh bubble by itself (the cancelled round's finalize resets
 * the segment state). */
export function appendInterjectedMessage(text: string): void {
  const cid = useFlux.getState().activeChatId;
  if (!cid) return;
  // The interjected message starts the replacement round — the artifact
  // list retires with the cancelled one.
  resetRound(cid);
  // Streaming is already live — the cancelled round holds the flag.
  appendUserBubble(cid, text);
}

/** The user bubble + TTFT indicator + force-follow (shared by both send
 * paths). The typing indicator survives the cancelled round's wrap-up and
 * is removed by the replacement round's first content (ensure and finalize
 * paths both remove it). */
function appendUserBubble(cid: string, text: string): void {
  const pane = ensurePane(cid);

  const bubble = createMessageBubble({ role: 'user', text });
  pane.appendChild(bubble.el);
  // TTFT feedback: the three-dot pulse until the first delta / tool card arrives (removed by ensure*/finalize)
  pane.appendChild(createTypingIndicator());

  // Sending a message = clearly wants to see the reply: force-follow to the bottom + stick (even if the user was reading above).
  forceFollow(pane);
  // The server counts the whole round (user + assistant + tool messages) —
  // the sidebar count refreshes from the authoritative chat_list push on
  // stream_end instead of a local guess.
}

/** Reconnect reset: the server-side chat loops died with the old
 * connection — dispose orphaned reveal controllers, clear streaming
 * flags, and forget loaded history so the chats handler re-opens the
 * active chat from the store. */
export function resetStreamingForReconnect(): void {
  clearInterrupt();
  clearAllRounds();
  for (const chatId of Object.keys(useFlux.getState().streaming)) {
    disposeController(chatId);
    useFlux.getState().clearStreaming(chatId);
  }
  useFlux.setState({ loadedChatId: '' });
  // Server leases/subscriptions reset with the old connection — clear stale
  // read-only marks or the input stays hidden behind a dead chat_busy.
  useFlux.setState({ readonlyChats: {} });
  resetAnnounce();
}
