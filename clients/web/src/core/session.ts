/**
 * session.ts — sessionStorage-backed identity continuity.
 *
 * The Subscribe stream's ready frame hands the connection the session id;
 * the client stores it and the stored token rides the NEXT stream open —
 * the server adopts it within the grace window (leases and subscriptions
 * survive), so a page refresh keeps its chat identity.
 *
 * sessionStorage is deliberate (NOT localStorage): it is per-tab, so two
 * tabs are two sessions — the lease model's "one operator per chat per
 * window" semantics depend on that — while a tab refresh re-uses its own
 * identity. All accessors swallow storage failures (private mode) so the
 * chat UI degrades to fresh-session behavior.
 *
 * Provides: readStoredSessionId, storeSessionId, readStoredActiveChat, storeActiveChat
 */
const SESSION_KEY = 'flux.session.id';
const ACTIVE_CHAT_KEY = 'flux.active.chat';

/** The previous connection's session id, if this tab has one. */
export function readStoredSessionId(): string | null {
  try {
    return sessionStorage.getItem(SESSION_KEY);
  } catch {
    return null;
  }
}

/** Persist the authoritative session id (from ready / session_resumed). */
export function storeSessionId(id: string): void {
  try {
    sessionStorage.setItem(SESSION_KEY, id);
  } catch {
    // storage unavailable — resume simply won't happen
  }
}

/** This tab's last active chat id, if any. */
export function readStoredActiveChat(): string | null {
  try {
    return sessionStorage.getItem(ACTIVE_CHAT_KEY);
  } catch {
    return null;
  }
}

/** Persist the active chat id (driven by an effect in mount). */
export function storeActiveChat(id: string): void {
  try {
    sessionStorage.setItem(ACTIVE_CHAT_KEY, id);
  } catch {
    // storage unavailable — focus restore simply won't happen
  }
}
