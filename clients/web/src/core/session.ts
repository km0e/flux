/**
 * session.ts — sessionStorage-backed identity continuity (D-19).
 *
 * The server hands the connection a session id in `ready`; the client
 * stores it and replays it via `session_resume` on the next connection, so
 * a page refresh keeps its leases and subscriptions.
 *
 * sessionStorage is deliberate (NOT localStorage): it is per-tab, so two
 * tabs are two sessions — the lease model's "one operator per chat per
 * window" semantics depend on that — while a tab refresh re-uses its own
 * identity. All accessors swallow storage failures (private mode) so the
 * chat UI degrades to the pre-D-19 fresh-session behavior.
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
