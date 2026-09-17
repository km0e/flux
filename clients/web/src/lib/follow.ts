/**
 * follow.ts — the stick-state scroll-follow machine.
 *
 * Scroll-following for streamed content is NOT "force-scroll on every
 * delta" but a stick state machine:
 *   - attached: scrolling up AND off the bottom (a 2px clamp tolerance)
 *     detaches, and detaching cancels the pending follow frame (a user
 *     scrolling up to read is never yanked back; a bottom clamp — content
 *     shrinking while pinned — reads as up-scroll but is NOT a detach)
 *   - detached: scrolling up stays detached; scrolling down into the bottom
 *     96px re-attaches automatically
 * Follow scrolls merge through requestAnimationFrame — N deltas in one frame
 * scroll once, with behavior equivalent to instant (smooth compounds latency
 * into wobble).
 *
 * The machine also drives USER-initiated content growth (tool-card /
 * reasoning-block expansion): while attached, the bottom stays pinned
 * across the CSS transition; while detached, the reader is never moved.
 *
 * Provides: isNearBottom, alwaysScrollToBottom, scrollPaneToBottom,
 *           forceFollow, scheduleFollow, followExpansion, followExpansionFrom
 * Depends: (none — the pane is passed in; the stick state rides WeakMaps)
 */

/** Near-bottom probes: the attach window (re-attach below) and the
 * "close enough to be at the bottom" test shared by the button logic. */

export function isNearBottom(el: HTMLElement, threshold = 50): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight < threshold;
}

/**
 * Scroll a pane to the bottom unconditionally (smooth). Used by the
 * scroll-to-bottom button — unlike {@link scrollPaneToBottom}, it must work
 * exactly when the user is NOT near the bottom.
 */
export function alwaysScrollToBottom(pane: HTMLElement): void {
  pane.scrollTo({ top: pane.scrollHeight, behavior: 'smooth' });
}

/**
 * Scroll a pane to the bottom only if it is already near the bottom —
 * respects the user's scroll position.
 */
export function scrollPaneToBottom(pane: HTMLElement, threshold: number): void {
  const st = followState(pane);
  if (isNearBottom(pane, threshold)) {
    st.stick = true; // a programmatic jump within the threshold re-attaches
    pane.scrollTop = pane.scrollHeight;
  }
}

// ── Stick state machine (rAF merge) ──────────────────────────────────────

const SCROLL_ENTER = 96;

/** Tolerance for the bottom-clamp test below (px): the clamp lands at
 * scrollTop + clientHeight == scrollHeight exactly, ±1 from integer
 * rounding on fractional-zoom devices. */
const CLAMP_TOLERANCE_PX = 2;

interface ScrollFollowState {
  stick: boolean;
  raf: number | null;
  lastTop: number;
}

const followStates = new WeakMap<HTMLElement, ScrollFollowState>();

function nearBottomPx(el: HTMLElement, threshold: number): boolean {
  return el.scrollTop + el.clientHeight >= el.scrollHeight - threshold;
}

function followState(pane: HTMLElement): ScrollFollowState {
  let st = followStates.get(pane);
  if (!st) {
    st = { stick: true, raf: null, lastTop: pane.scrollTop };
    followStates.set(pane, st);
    pane.addEventListener(
      'scroll',
      () => {
        const goingUp = pane.scrollTop < st!.lastTop - 1;
        if (st!.stick) {
          // While attached, a scroll event has only THREE sources: the follow
          // scroll itself (programmatic, including the reposition after a burst
          // of content growth), the user scrolling DOWN, or a CLAMP — content
          // SHRINKING while pinned at the bottom (an incremental streaming
          // render whose boundary frame nets negative: paragraph commit
          // reflow, code-fence fold-back removing committed nodes) pulls
          // scrollTop down to the new scrollHeight - clientHeight and
          // dispatches a scroll event. A clamp lands EXACTLY on the bottom,
          // so an event that is both goingUp AND still pinned to the bottom
          // is the renderer shrinking, not the reader leaving — detaching
          // here kills the follow mid-stream (the expanded-reasoning bug:
          // every later delta's scheduleFollow no-ops behind the stick gate
          // until the user manually scrolls back). A real user scroll-up
          // leaves the bottom by more than the tolerance at once, or on the
          // very next wheel tick — the worst case is a one-event detach lag.
          if (goingUp && !nearBottomPx(pane, CLAMP_TOLERANCE_PX)) {
            st!.stick = false;
            if (st!.raf !== null) {
              cancelAnimationFrame(st!.raf);
              st!.raf = null;
            }
          }
        } else if (!goingUp && nearBottomPx(pane, SCROLL_ENTER)) {
          st!.stick = true;
        }
        st!.lastTop = pane.scrollTop;
      },
      { passive: true },
    );
  }
  return st;
}

/**
 * Force back to the bottom and re-attach — a "clearly wants to see the
 * reply" intent like sending a message.
 */
export function forceFollow(pane: HTMLElement): void {
  const st = followState(pane);
  st.stick = true;
  if (st.raf !== null) {
    cancelAnimationFrame(st.raf);
    st.raf = null;
  }
  pane.scrollTop = pane.scrollHeight;
}

/**
 * Streaming follow: while attached, coalesce scroll-to-bottom through rAF
 * (at most one per frame); ignored when detached. Each per-delta call is
 * cheap — the actual scroll happens at most once per frame.
 */
export function scheduleFollow(pane: HTMLElement): void {
  const st = followState(pane);
  if (!st.stick || st.raf !== null) return;
  st.raf = requestAnimationFrame(() => {
    st.raf = null;
    if (!st.stick) return;
    pane.scrollTop = pane.scrollHeight;
  });
}

// ── Expansion follow (user-opened tool cards / reasoning blocks) ─────────
//
// The pane is the scroll container and carries `overflow-anchor: none`
// (native anchoring is disabled on purpose), and expansion grows content
// through a ~180ms CSS grid-rows transition — without an explicit follow
// the revealed content lands below the fold and the scrollbar thumb just
// rises. User-triggered DOM growth must drive the same stick machine the
// streamed deltas do:
//   - attached (stick): the viewport is at the bottom, so the expanded
//     element is necessarily near it — keep the bottom pinned across the
//     transition with a bounded rAF loop (each frame merges through
//     scheduleFollow; the stick machine self-cancels when the user
//     scrolls up mid-animation).
//   - detached: the user is reading history — never yank them; scrolling
//     down into the re-attach window is their own move.

/** Follow-window bounds (wall-clock ms). The MINIMUM also covers
 * transition-less expansions (the reasoning <details> opens instantly);
 * the MAXIMUM keeps a pathological computed duration from pinning the
 * loop for seconds. */
const EXPANSION_FOLLOW_MIN_MS = 250;
const EXPANSION_FOLLOW_MAX_MS = 1000;
/** Margin over the computed transition duration — the loop must outlive
 * the growth so the final write lands on the SETTLED height. */
const EXPANSION_FOLLOW_MARGIN_MS = 120;

/**
 * Total CSS transition time of one element (the max across its
 * transitioned properties, e.g. the tool-detail's grid-rows + opacity
 * pair), in ms. 0 when nothing transitions.
 */
function transitionMs(el: HTMLElement): number {
  let max = 0;
  for (const token of getComputedStyle(el).transitionDuration.split(',')) {
    const t = token.trim();
    const v = parseFloat(t);
    if (Number.isNaN(v)) continue;
    const ms = t.endsWith('ms') ? v : v * 1000;
    if (ms > max) max = ms;
  }
  return max;
}

/**
 * Follow a user-initiated expansion (tool card / reasoning details) so the
 * revealed content does not grow below the fold unnoticed.
 *
 * The loop is bounded in WALL TIME, never in frames — a frame counter
 * assumes 60fps, and on a 120/144Hz display 16 frames end BEFORE the
 * 180ms detail transition: the follow stops mid-growth and the pane lands
 * above the true bottom (the "it only reached the expanded content's
 * bottom" bug). Deriving the deadline from the element's own computed
 * transition duration keeps the loop spanning the growth at ANY refresh
 * rate; frames past the transition end are no-op writes on the settled
 * height, so the final state is exactly the bottom.
 *
 * Stick semantics unchanged: attached = keep the bottom pinned across the
 * transition (each frame merges through scheduleFollow and self-cancels
 * when the user scrolls up); detached = never yank. Collapse shrinks the
 * content — the clamp-down scroll detaches the stick state and the loop
 * degrades to no-ops on its own.
 */
export function followExpansion(pane: HTMLElement, durationMs = EXPANSION_FOLLOW_MIN_MS): void {
  const st = followState(pane);
  if (!st.stick) return;
  const deadline = performance.now() + durationMs;
  const tick = () => {
    scheduleFollow(pane); // no-op once detached (user scrolled up mid-transition)
    if (st.stick && performance.now() < deadline) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
}

/** Expansion follow anchored on the expanded element — resolves the pane
 * through the DOM (`.chat-pane` is the cross-layer scroll-container
 * contract) and the follow window from the element's own transition (the
 * tool card's `.tool-detail` animates grid-template-rows; the reasoning
 * <details> opens instantly and falls back to the minimum window). No
 * pane (detached node, test fixture): a no-op. */
export function followExpansionFrom(el: HTMLElement): void {
  const pane = el.closest<HTMLElement>('.chat-pane');
  if (!pane) return;
  const detail = el.querySelector<HTMLElement>('.tool-detail') ?? el;
  const duration = Math.min(
    Math.max(transitionMs(detail) + EXPANSION_FOLLOW_MARGIN_MS, EXPANSION_FOLLOW_MIN_MS),
    EXPANSION_FOLLOW_MAX_MS,
  );
  followExpansion(pane, duration);
}
