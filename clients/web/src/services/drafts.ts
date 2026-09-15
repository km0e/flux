/**
 * drafts.ts — Per-chat composer draft preservation.
 *
 * ChatInput remounts on every chat switch (key={cid} in ChatView), which
 * wipes its uncontrolled textarea. Drafts give each chat its own pending
 * composer text: saved when the composer unmounts (a switch away), restored
 * on mount, cleared on send, removed when the chat is deleted.
 *
 * Storage: an in-memory LRU write-through to sessionStorage — a page reload
 * restores drafts too. Writes happen ONLY on save/clear, never per
 * keystroke. The LRU cap is required: ids never recycle, so an uncapped map
 * would grow with every chat ever visited in the tab.
 *
 * Cleanup timing: deletion paths call deleteDraft / pruneDrafts — NOT
 * clearChatPane (services/panes.ts evictOverflow also goes through it; LRU
 * pane eviction ≠ chat deletion, and the draft must survive an eviction).
 *
 * Provides: getDraft, saveDraft, clearDraft, deleteDraft, pruneDrafts
 */

/** Cap on live drafts (mirrors panes.ts MAX_LIVE_PANES discipline). */
const MAX_DRAFTS = 64;
const PREFIX = 'flux:draft:';

/** Drafts keyed by chat id — Map iteration order IS the LRU order
 * (re-insertion on access moves an entry to the recency end). */
const drafts = new Map<string, string>();

function readSession(chatId: string): string | undefined {
  try {
    return sessionStorage.getItem(PREFIX + chatId) ?? undefined;
  } catch {
    return undefined; // storage unavailable — memory-only fallback
  }
}

function writeSession(chatId: string, text: string): void {
  try {
    if (text) sessionStorage.setItem(PREFIX + chatId, text);
    else sessionStorage.removeItem(PREFIX + chatId);
  } catch {
    /* quota/private mode — the in-memory copy still covers the session */
  }
}

/** Drop the oldest entries beyond the cap, memory AND session storage
 * (keeping evicted entries in storage would let getDraft rehydrate them,
 * defeating the cap). */
function evictOverflow(): void {
  while (drafts.size > MAX_DRAFTS) {
    const oldest = drafts.keys().next().value;
    if (oldest === undefined) break;
    drafts.delete(oldest);
    writeSession(oldest, '');
  }
}

/** The chat's pending composer text ('' when none). */
export function getDraft(chatId: string): string {
  if (!chatId) return '';
  const hit = drafts.get(chatId);
  if (hit !== undefined) return hit;
  const stored = readSession(chatId);
  if (stored) {
    // Rehydrate into the LRU (re-insertion = recency touch).
    drafts.set(chatId, stored);
    evictOverflow();
  }
  return stored ?? '';
}

/** Save the pending text. Empty text is a no-op — an empty composer has
 * nothing to preserve (clearing goes through clearDraft). */
export function saveDraft(chatId: string, text: string): void {
  if (!chatId || !text) return;
  drafts.delete(chatId);
  drafts.set(chatId, text);
  writeSession(chatId, text);
  evictOverflow();
}

/** The draft was sent (or superseded) — drop it everywhere. */
export function clearDraft(chatId: string): void {
  if (!chatId) return;
  drafts.delete(chatId);
  writeSession(chatId, '');
}

/** The chat is gone — ids never recycle, so its draft is dead weight. */
export function deleteDraft(chatId: string): void {
  clearDraft(chatId);
}

/** Drop drafts of chats that no longer exist (the authoritative chats
 * broadcast). Enumerates drafts directly — a draft whose pane was
 * LRU-evicted is cleaned too, which the pane-id loop in handlers.ts
 * cannot see. */
export function pruneDrafts(liveIds: Set<string>): void {
  for (const chatId of [...drafts.keys()]) {
    if (!liveIds.has(chatId)) deleteDraft(chatId);
  }
}
