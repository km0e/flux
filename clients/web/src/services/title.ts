/**
 * title.ts — the document title as a state surface.
 *
 * A static `<title>Flux</title>` gives the browser tab / PWA switcher /
 * window list nothing: which conversation, is a round running, did
 * something finish while the tab was in the background. The title
 * composes THREE facts, written ONLY on transitions (never per delta —
 * text deltas would thrash the title tens of times a second):
 *
 *   ({events}) {name | Flux}{ — working…}{ — Flux}
 *
 *   - `events` — background attention: rounds finishing or questions
 *     arriving while the tab was HIDDEN (a visible window already shows
 *     panes and toasts; counting there is noise). Cleared when the page
 *     becomes visible again — the browser-tab convention.
 *   - `working…` — the active conversation is streaming.
 *   - the conversation name, with the app identity anchored last.
 *
 * Provides: composeTitle, syncTitle, initTitleAttention
 * Depends: core/state.ts
 */
import { useFlux } from '../core/state';

const BASE_TITLE = 'Flux';

/** Pure composition (exported for tests). */
export function composeTitle(input: {
  streaming: boolean;
  name?: string;
  events: number;
}): string {
  const prefix = input.events > 0 ? `(${input.events}) ` : '';
  const subject = input.name || BASE_TITLE;
  const state = input.streaming ? ' — working…' : '';
  const tail = input.name ? ` — ${BASE_TITLE}` : '';
  return `${prefix}${subject}${state}${tail}`;
}

/** Read the store, write the title. Called on state transitions and once
 * at mount. */
export function syncTitle(): void {
  const s = useFlux.getState();
  const streaming = s.activeChatId ? (s.streaming[s.activeChatId] ?? false) : false;
  const name = s.chats.find((c) => c.id === s.activeChatId)?.name || undefined;
  document.title = composeTitle({ streaming, name, events: s.backgroundEvents });
}

let attentionInstalled = false;

/** The attention counter clears when the page becomes visible again (the
 * user is back — the count has served its purpose). Idempotent. */
export function initTitleAttention(): void {
  if (attentionInstalled) return;
  attentionInstalled = true;
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) useFlux.getState().clearBackgroundEvents();
  });
}
