/**
 * history.ts — Complete chat-history rendering into a pane.
 *
 * Receives `chat_history` payloads from the server and constructs the
 * static message DOM (user/assistant bubbles, reasoning blocks, tool cards).
 *
 * Provides: renderHistoryMessages, attachForkToLiveBubble
 * Depends: lib/markdown.ts, services/panes.ts, services/stream.ts,
 *          services/forkDraft.ts, lib/dom.ts, core/types.ts
 */

import { log } from '../logger';
import { preloadHighlighter } from '../lib/highlight';
import { renderMarkdown } from '../lib/markdown';
import { getPane, getPaneIfExists, hideEmptyState } from './panes';
import { stashForkDraft } from './forkDraft';
import { takePaneStale } from './stream-handler';
import { disposeController } from './stream';
import { rebuildRoundFromHistory } from './artifacts';
import { bridge } from '../core/bridge';
import {
  buildForkButton,
  createMessageBubble,
  createReasoningBlock,
  createToolCard,
  escapeCssSelector,
  forceFollow,
  setToolCardResult,
  toolIconSvg,
} from '../lib/dom';
import type { HistoryMessage } from '../core/types';
import { useFlux } from '../core/state';

// ── History pagination (tail-first) ──────────────────────────────────────
//
// A coding-agent transcript runs to hundreds of messages; building every
// one (markdown parse + highlight + DOM) synchronously on chat open blocks
// the main thread for as long. Instead the FIRST render lays down only the
// LAST page — the view opens at the bottom anyway (forceFollow) — and a
// "load earlier" affordance prepends older pages on demand.
//
// Page boundaries align to USER messages: a page always starts at a user
// bubble, so the (assistant tool_calls → tool result) pairs of a round
// never straddle a boundary (a split pair would render the result as an
// orphan card and the later-loaded call card without its result).

/** Messages per page — the initial tail AND each earlier page. */
const HISTORY_PAGE = 60;

interface HistoryCursor {
  messages: HistoryMessage[];
  /** Index of the first message currently rendered in the pane. */
  from: number;
}

/** Cursor keyed by the PANE element (not the chat id): pane removal GCs it
 * automatically, and a wiped pane (stale resync) only reaches it through
 * the button that wipe just removed — no stale-cursor path exists. */
const historyCursors = new WeakMap<HTMLDivElement, HistoryCursor>();

/**
 * Align a candidate page start backwards to the nearest user message —
 * rounds stay atomic (see module comment). Index 0 needs no alignment.
 * Pure; exported for tests.
 */
export function historyPageStart(messages: HistoryMessage[], candidate: number): number {
  let from = Math.max(0, Math.min(candidate, messages.length));
  while (from > 0 && messages[from]?.role !== 'user') from--;
  return from;
}

/** The "load earlier" affordance prepended above the oldest rendered page. */
function buildLoadEarlierButton(remaining: number): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'history-load-earlier';
  btn.textContent = `Load earlier messages (${remaining})`;
  return btn;
}

/**
 * Render one history message into `mount` — the shared body of the
 * initial tail and every earlier page (identical semantics: fork
 * affordance, reasoning block, markdown body, tool-call cards, result
 * merge by call id). `pane` stays the QUERY root (the matching call card
 * may sit anywhere in the pane) while `mount` receives the nodes — the
 * earlier-page path builds into a fragment so one insert lands above the
 * current page.
 */
function appendHistoryMessage(
  pane: HTMLDivElement,
  mount: ParentNode,
  chatId: string,
  msg: HistoryMessage,
): void {
  switch (msg.role) {
    case 'user': {
      // Fork affordance: history row ids are the fork points. The only
      // gate is the id itself (assembled messages carry none) — forking
      // is a non-destructive read + create, so read-only viewers may
      // fork too.
      const bubble = createMessageBubble({
        role: 'user',
        text: msg.content,
        forkPoint: msg.id,
      });
      if (msg.id !== undefined) attachForkBehavior(bubble.el, chatId, msg.id, msg.content ?? '');
      mount.appendChild(bubble.el);
      break;
    }
    case 'assistant': {
      // Reasoning block
      if (msg.reasoning_content) {
        const block = createReasoningBlock({
          summary: 'Thought for a bit',
          html: renderMarkdown(msg.reasoning_content),
        });
        block.el.open = false;
        mount.appendChild(block.el);
      }

      // Assistant message body — reasoning-only rounds (e.g. thinking +
      // tool calls with no prose) have empty content: no bubble at all,
      // or a hollow shell renders under the thinking block.
      if (msg.content) {
        const bubble = createMessageBubble({
          role: 'assistant',
          html: renderMarkdown(msg.content),
          raw: msg.content, // enables createCopyButton to read the raw text
        });
        mount.appendChild(bubble.el);
      }

      // Tool call cards — `tool_calls` is absent on the wire when empty
      // (the server skips serializing empty vecs).
      for (const tc of msg.tool_calls ?? []) {
        const toolEl = createToolCard({
          id: tc.id,
          name: tc.name,
          args: tc.arguments,
          status: 'done',
        });
        mount.appendChild(toolEl);
      }
      break;
    }
    case 'tool': {
      const result = msg.content ?? '';
      // Merge into the already-rendered call card first (assistant
      // tool_calls built cards by id) — consistent with the live path
      // (tool_start builds the card, tool_result fills it): call + result in
      // ONE card instead of an extra anonymous orphan "result" card.
      const callCard = msg.tool_call_id
        ? pane.querySelector<HTMLElement>(
            `[data-tool-call-id="${escapeCssSelector(msg.tool_call_id)}"]`,
          )
        : null;
      if (callCard) {
        setToolCardResult(callCard, result);
        break;
      }
      // Fallback: no matching call card (truncated history / result-only message) → a standalone result card.
      const toolEl = createToolCard({
        id: msg.tool_call_id || '',
        name: '',
        args: undefined,
        status: 'done',
      });
      // Override tool card appearance for inline tool results — same
      // icon system as createToolCard, anonymous "result" status.
      const header = toolEl.querySelector('.tool-header') as HTMLElement;
      if (header) {
        header.innerHTML =
          '<span class="tool-icon">' +
          toolIconSvg('result') +
          '</span>' +
          '<span class="tool-args-summary"></span>' +
          '<span class="tool-status completed"> result</span>';
        header.setAttribute('aria-label', 'Tool result');
      }
      // setToolCardResult fills the result container (meta + copy
      // button; the <pre> materializes on first expansion) — the same
      // shape the matched-card path gets.
      if (result) setToolCardResult(toolEl, result);
      mount.appendChild(toolEl);
      break;
    }
  }
}

/**
 * Prepend the previous page above the oldest rendered one. The page
 * builds into a fragment (ONE insert into the live pane), and the scroll
 * anchor is preserved explicitly: prepending grows scrollHeight without
 * moving scrollTop, which would visually JUMP the reader down a page —
 * compensate by the exact height delta. The stick machine is detached
 * here (the user scrolled UP to reach the button), so no follow
 * interference.
 */
function renderEarlierPage(pane: HTMLDivElement, chatId: string): void {
  const cursor = historyCursors.get(pane);
  if (!cursor || cursor.from <= 0) return;
  const target = Math.max(0, cursor.from - HISTORY_PAGE);
  const from = historyPageStart(cursor.messages, target);
  const page = cursor.messages.slice(from, cursor.from);

  const button = pane.querySelector('.history-load-earlier');
  const prevHeight = pane.scrollHeight;
  const frag = document.createDocumentFragment();
  for (const msg of page) appendHistoryMessage(pane, frag, chatId, msg);
  pane.insertBefore(frag, button ?? pane.firstChild);
  cursor.from = from;
  // Re-anchor: add the grown height so the previously-top message stays
  // where it was.
  pane.scrollTop += pane.scrollHeight - prevHeight;
  if (from === 0) button?.remove();
  else if (button) button.textContent = `Load earlier messages (${from})`;
}

// ── History rendering ──

/**
 * Render complete chat history messages into a pane.
 * Called when the server sends `chat_history` in response to a first
 * `chat_open` subscription or a `chat_claim` (the single-message claim).
 */
export async function renderHistoryMessages(
  chatId: string,
  messages: HistoryMessage[],
): Promise<void> {
  // The highlighter is a deferred async chunk prefetched at boot; awaiting
  // it here is the one place the tiny load window is realistic (history can
  // render before the prefetch lands) — without the wait, code blocks would
  // render plain and, being committed-once, stay plain until re-open.
  await preloadHighlighter();
  log.debug('renderHistory ' + chatId + ' msgs=' + messages.length);
  // Safety net: never tear down a live round. If this chat is actively
  // streaming, the live stream is rendering newer content — rebuilding from
  // a history snapshot would dispose the controller and clear the pane,
  // dropping in-flight frames. (The server sends history before subscribing
  // on ChatOpen, so this is a race/duplicate-open path, not the norm.)
  // OVERRIDE: a STALE pane (the session departed mid-round — see
  // stream-handler's stalePanes) must re-render even though the streaming
  // flag may still say live: the flag went stale across the unsubscribe
  // window, and everything the round persisted after departure is missing
  // from the DOM. The snapshot is the resync; live deltas continue from it.
  const paneStale = takePaneStale(chatId);
  if (useFlux.getState().streaming[chatId] && !paneStale) {
    log.debug('renderHistory skipped: ' + chatId + ' is streaming');
    return;
  }
  // Dispose any active stream controller for this chat
  disposeController(chatId);

  // The round artifact list rebuilds from the same snapshot (F-11): the
  // last user message starts the current round. Same skip rule as the
  // DOM above — a live round's list is already authoritative.
  rebuildRoundFromHistory(chatId, messages);

  const pane = getPane(chatId);

  // Clear existing message DOM (keep empty state element)
  for (const child of Array.from(pane.children)) {
    if (!child.id || !child.id.startsWith('empty-')) {
      child.remove();
    }
  }

  if (messages.length > 0) {
    hideEmptyState(chatId);
  }

  // Tail-first pagination: the FIRST render lays down only the LAST page
  // (the view opens at the bottom anyway — forceFollow below); older
  // pages prepend on demand via the "load earlier" affordance. The
  // artifacts rebuild above already consumed the FULL snapshot, so the
  // round list is unaffected by what the DOM shows.
  const from = historyPageStart(messages, Math.max(0, messages.length - HISTORY_PAGE));
  const page = messages.slice(from);
  historyCursors.set(pane, { messages, from });

  if (from > 0) {
    const btn = buildLoadEarlierButton(from);
    btn.addEventListener('click', () => renderEarlierPage(pane, chatId));
    pane.appendChild(btn);
  }

  // NOTE: no per-message stagger here. The stagger delays exist to soften
  // the LIVE stream's burst; a history RESTORE is a resync of content that
  // already exists — with fill-mode:both a delayed message sits at opacity 0,
  // and the delay is largest for the LAST messages, exactly where the view
  // opens (forceFollow lands at the bottom). The result read as "scrolled to
  // the bottom but blank for a moment". One simultaneous msgIn fade instead.
  for (const msg of page) {
    appendHistoryMessage(pane, pane, chatId, msg);
  }

  if (messages.length > 0) {
    // Opening a chat means looking at the latest message: scroll to the
    // bottom + reset stick, so the live follow is not lost afterwards.
    // AFTER the appends — the scroll must land on the grown content. A
    // pre-append call lands on the just-cleared pane (scrollTop 0), the
    // view stays parked at the TOP of the history — the first few
    // messages paint and only a later follow (a live delta, a send)
    // jumps to the bottom, reading as "rendered a few, got interrupted,
    // then fully re-rendered".
    forceFollow(pane);
  }
}

/** Fork from a message: NO confirmation — a fork is a non-destructive
 * read + create (the source chat is untouched), so the click sends
 * directly. The copy EXCLUDES the fork point (it is the turn being
 * redone), so the click stashes its content; the ack's chat_created
 * pairs it with the fork and the fork's composer opens prefilled (see
 * forkDraft.ts). The ack's auto-select lands the fork; its claim
 * delivers the copied transcript. */
function forkFrom(chatId: string, messageId: number, content: string): void {
  stashForkDraft(chatId, content);
  bridge.send({ type: 'fork', chat_id: chatId, fork_point: messageId });
}

/** Wire the click behavior onto a bubble that already carries the
 * `.msg-fork` button (history render). */
function attachForkBehavior(
  bubble: HTMLElement,
  chatId: string,
  messageId: number,
  content: string,
): void {
  bubble
    .querySelector<HTMLButtonElement>('.msg-fork')
    ?.addEventListener('click', () => forkFrom(chatId, messageId, content));
}

/** Live path: the sender's OWN user bubble just persisted with `id` — find
 * the matching un-id'd live bubble in the pane (exact text, oldest first)
 * and attach the affordance. Returns whether a bubble matched. A cancelled
 * turn never persisted → its bubble never matches → the affordance never
 * appears on it, which is honest (there is no row to fork at). */
export function attachForkToLiveBubble(chatId: string, messageId: number, content: string): boolean {
  const pane = getPaneIfExists(chatId);
  if (!pane) return false;
  const candidates = [...pane.querySelectorAll<HTMLElement>('.message.user')].filter(
    (el) =>
      !el.querySelector('.msg-fork') &&
      (el.querySelector('.message-body')?.textContent ?? '') === content,
  );
  const target = candidates[0];
  if (!target) return false;
  target.insertBefore(buildForkButton(messageId), target.querySelector('.message-body'));
  attachForkBehavior(target, chatId, messageId, content);
  return true;
}
