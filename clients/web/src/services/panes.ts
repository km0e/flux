/**
 * panes.ts — Per-chat DOM pane management.
 *
 * Provides: getPaneIfExists, getPane, hideEmptyState, clearPaneMessages,
 *           ensurePane, switchToChat, removePane, clearChatPane,
 *           getPaneIds, _resetPanesForTest
 * Depends: core/state.ts, lib/dom.ts, services/stream.ts
 * Note: DO NOT delete — Sidebar, MessageList and stream-handler depend on
 * these exports.
 */

import { log } from '../logger';
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { disposeController } from './stream';
import { isNearBottom } from '../lib/dom';
import { dismissPendingQuestion } from './dialogs';

const panes = new Map<string, HTMLDivElement>();

/** The brand wave, inline (no tile, no gradient — the mark alone). */
const WAVE_SVG =
  '<svg viewBox="22 22 84 46" width="52" height="29" fill="currentColor" aria-hidden="true">' +
  '<path d="M 8 28 C 32 28, 48 56, 64 56 C 80 56, 96 44, 120 44 L 120 52 ' +
  'C 96 52, 80 64, 64 64 C 48 64, 32 44, 8 44 Z"/></svg>';

/** First-round prompt suggestions — real, specific entries; clicking one
 * fills the composer (ChatInput listens for `flux:compose`) and focuses
 * it. Copy speaks from the user's side, in plain verbs. */
const PROMPT_SUGGESTIONS = [
  'Map this repo and explain how it fits together',
  'Find and fix the failing tests',
  'Review my latest changes before I commit',
];

/** Cap on live chat panes. Each pane holds its full history DOM — an
 * unbounded set grows with every conversation visited in one session.
 * Map iteration order is insertion order, so re-inserting on access makes
 * `panes` itself the LRU order; overflow evicts the least recently used
 * pane that is neither active nor streaming. A revisit re-claims (the
 * existing `chats`-handler reopen check) and re-pulls history from the
 * store — correctness is the reload path, cost is one round-trip. */
const MAX_LIVE_PANES = 8;

/**
 * Get a chat's pane without creating one. Returns undefined when the pane
 * does not exist (e.g. a late tool_result after the chat was deleted) — a
 * fresh pane with an empty state must not materialize out of thin air.
 */
export function getPaneIfExists(chatId: string): HTMLDivElement | undefined {
  return panes.get(chatId);
}

export function getPane(chatId: string): HTMLDivElement {
  let pane = panes.get(chatId);
  if (pane) {
    // LRU touch — re-insertion moves the pane to the recency end.
    panes.delete(chatId);
    panes.set(chatId, pane);
    return pane;
  }

  const wrap = document.getElementById('messages-wrap');
  if (!wrap) throw new Error('messages-wrap not found');

  pane = document.createElement('div');
  // overflow-anchor: none lives in stream.css `.chat-pane` — Tailwind
  // generates no overflow-anchor utility (the class here was dead).
  pane.className = 'chat-pane flex-1 overflow-y-auto flex flex-col pb-2';
  pane.dataset.chatId = chatId;

  // Empty state for this pane — an invitation to act: the wave mark, a
  // plain-language heading, and prompt entries that fill the composer.
  const empty = document.createElement('div');
  empty.id = `empty-${chatId}`;
  empty.className = 'fx-empty-state';
  empty.innerHTML =
    '<div class="fx-empty-mark" aria-hidden="true">' +
    WAVE_SVG +
    '</div>' +
    '<div class="fx-empty-title">What should Flux work on?</div>' +
    '<div class="fx-empty-sub">Flux runs tools inside this chat’s project directory.</div>' +
    '<div class="fx-empty-prompts">' +
    PROMPT_SUGGESTIONS.map(
      (q) =>
        '<button type="button" class="fx-empty-prompt" data-prompt="' +
        // constant strings — no user input; still escape for safety
        q.replace(/&/g, '&amp;').replace(/"/g, '&quot;') +
        '">' +
        q +
        '</button>',
    ).join('') +
    '</div>';
  for (const btn of empty.querySelectorAll<HTMLButtonElement>('.fx-empty-prompt')) {
    btn.addEventListener('click', () => {
      window.dispatchEvent(
        new CustomEvent('flux:compose', { detail: btn.dataset.prompt ?? '' }),
      );
    });
  }
  pane.appendChild(empty);

  // Scroll listener for scroll-to-bottom button — visible whenever the
  // active pane is away from the bottom (not gated on streaming).
  pane.addEventListener(
    'scroll',
    () => {
      const p = panes.get(chatId);
      if (!p) return;
      if (p.dataset.chatId === useFlux.getState().activeChatId) {
        // setState, never a direct field write — direct assignment mutates
        // without notifying, so the MessageList subscription never fires
        // and the button stops reacting to user scrolls. zustand re-renders
        // only when the selected slice changes, so per-scroll-event writes
        // are free.
        useFlux.setState({ scrollBtnVisible: !isNearBottom(p, 80) });
      }
    },
    { passive: true },
  );

  wrap.appendChild(pane);
  panes.set(chatId, pane);
  evictOverflow();
  return pane;
}

/** Drop the oldest panes beyond the cap — never the active chat or one
 * with a live round. `chat_close` keeps the server-side view consistent
 * (unsubscribe; a non-held lease is a no-op), so a revisit re-claims and
 * re-pulls history exactly like a fresh page load. */
function evictOverflow(): void {
  const { activeChatId, streaming } = useFlux.getState();
  for (const id of panes.keys()) {
    if (panes.size <= MAX_LIVE_PANES) break;
    if (id === activeChatId || streaming[id]) continue;
    log.debug('pane evict (LRU) ' + id);
    bridge.send({ type: 'chat_close', chat_id: id });
    clearChatPane(id);
  }
}

export function hideEmptyState(chatId: string): void {
  const empty = document.getElementById(`empty-${chatId}`);
  if (empty) empty.style.display = 'none';
}

/** Wipe a pane's message DOM back to the empty state. Used by the switch
 * path for a STALE pane (departed mid-round — see stream-handler's
 * stalePanes): the outdated messages must not flash while the claim's
 * history snapshot is in flight. The controller is left alone — the
 * snapshot render owns its disposal. A pending question card dies with
 * the wipe — its promise resolves as dismissed (registry), never hangs. */
export function clearPaneMessages(chatId: string): void {
  dismissPendingQuestion(chatId);
  const pane = getPaneIfExists(chatId);
  if (!pane) return;
  for (const child of Array.from(pane.children)) {
    if (!child.id || !child.id.startsWith('empty-')) {
      child.remove();
    }
  }
  const empty = document.getElementById(`empty-${chatId}`);
  if (empty) empty.style.display = '';
}

/** Get (creating if needed) a chat's pane and hide its empty state. */
export function ensurePane(chatId: string): HTMLDivElement {
  const pane = getPane(chatId);
  hideEmptyState(chatId);
  return pane;
}

export function switchToChat(chatId: string): void {
  log.debug('switchToChat ' + chatId);
  // Fade out current pane before switching
  for (const [id, pane] of panes) {
    if (id === chatId) continue;
    if (pane.style.display !== 'none') {
      pane.classList.add('switching-out');
      setTimeout(() => {
        // A stale timer from rapid A→B→A switching must not hide the
        // pane that is active when it fires.
        if (pane.dataset.chatId === useFlux.getState().activeChatId) {
          pane.classList.remove('switching-out');
          return;
        }
        pane.style.display = 'none';
        pane.classList.remove('switching-out');
      }, 120);
    }
  }

  // Show and fade in the target pane
  if (chatId) {
    const target = getPane(chatId);
    // Pane may still carry the fade-out class if it was re-selected mid-switch
    target.classList.remove('switching-out');
    target.style.opacity = '0';
    target.style.display = '';
    void target.offsetHeight; // force reflow so the CSS transition fires
    target.style.opacity = ''; // clear inline opacity; CSS rule (opacity 1) takes over
  } else {
    for (const [, pane] of panes) {
      pane.style.display = 'none';
    }
  }
}

/**
 * Remove a chat's pane element and registry entry entirely.
 * Used when a conversation is deleted.
 */
export function removePane(chatId: string): void {
  const pane = panes.get(chatId);
  if (!pane) return;
  pane.remove();
  panes.delete(chatId);
}

/**
 * Remove all message DOM and per-chat records for a conversation.
 * Called when the user deletes a conversation, and when the authoritative
 * chats list shows it was deleted elsewhere (pane pruning).
 */
export function clearChatPane(chatId: string): void {
  log.debug('clearChatPane ' + chatId);
  dismissPendingQuestion(chatId);
  disposeController(chatId);
  removePane(chatId);
  // disposeController clears streaming/reasoning; prune the usage and
  // readonly marks here too (the external-deletion path bypasses
  // state.deleteChat).
  const nextUsage = { ...useFlux.getState().usage };
  delete nextUsage[chatId];
  useFlux.setState({ usage: nextUsage });
  useFlux.getState().setReadOnly(chatId, false);
}

/** Ids of all live panes. Lets callers prune panes whose chat no longer exists. */
export function getPaneIds(): string[] {
  return [...panes.keys()];
}

/** Reset the pane registry. For test isolation only. */
export function _resetPanesForTest(): void {
  for (const [, pane] of panes) {
    pane.remove();
  }
  panes.clear();
}
