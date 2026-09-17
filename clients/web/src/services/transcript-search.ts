/**
 * transcript-search.ts — in-transcript search over the ACTIVE chat's
 * rendered pane (Ctrl/Cmd+F).
 *
 * Contract respect: the streaming pipeline's DOM is append-only and
 * React-hostile — the search NEVER mutates it. Matches are carried by the
 * CSS Custom Highlight API (Highlight + `::highlight()` in stream.css),
 * which paints over existing text nodes from the outside; registration is
 * skipped on engines without the API (jsdom tests still exercise the
 * scan/navigation math; browsers without it never see the Ctrl+F
 * intercept and keep native find).
 *
 * Matching is CROSS-NODE: the pane's eligible text nodes are joined into
 * one haystack (absolute offsets mapped back per node), so a query finds
 * phrases that run across inline boundaries ("ran <code>fine</code>" →
 * "ran fine") exactly like a native find bar. Case-insensitive,
 * non-overlapping, in document order.
 *
 * Scope: the RENDERED transcript only — history is tail-paginated, older
 * pages join the index when loaded (the mutation-driven rescan picks them
 * up). Streaming appends rescan via a debounced MutationObserver that
 * clamps (never re-scrolls — the view must not yank while a round writes).
 *
 * Provides: searchSupported, openSearch, closeSearch, setQuery,
 *           searchQuery, nextMatch, prevMatch, _resetSearchForTest
 * Depends: services/panes.ts, core/state.ts
 */
import { getPaneIfExists } from './panes';
import { useFlux } from '../core/state';

const NAME_ALL = 'flux-search';
const NAME_CURRENT = 'flux-search-current';
/** Streaming bursts coalesce into ONE rescan per tick. */
const RESCAN_DEBOUNCE_MS = 150;

/** The Custom Highlight API gate — BOTH halves or nothing: the registry
 * to paint with AND the Highlight type to paint it with. */
export function searchSupported(): boolean {
  return typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight === 'function';
}

// ── Module state (one live search; the store carries only the projection) ──

let query = '';
let ranges: Range[] = [];
let current = 0; // 0-based; clamped by every rescan
let observer: MutationObserver | null = null;
let observedPane: HTMLDivElement | null = null;
let rescanTimer: number | null = null;
/** The chat the live search was opened FOR — the bar's switch-close
 * effect compares against THIS (the service's truth), not a mount-time
 * ref: a batched "switch + open" commit must not close what it opened. */
let openedCid = '';

/** Open the bar: attach the observer to the active pane and restore the
 * session's query (rescan publishes the counts; the view is NOT scrolled
 * — reopening a find bar must not yank the transcript). */
export function openSearch(): void {
  const { activeChatId } = useFlux.getState();
  const pane = activeChatId ? getPaneIfExists(activeChatId) : undefined;
  if (!pane) return;
  openedCid = activeChatId;
  attach(pane);
  useFlux.getState().setSearchOpen(true);
  scan(false);
}

/** The chat the live search belongs to ('' when none is open). */
export function searchOpenedForCid(): string {
  return openedCid;
}

export function closeSearch(): void {
  detach();
  ranges = [];
  current = 0;
  registerHighlights();
  const st = useFlux.getState();
  if (st.searchOpen) st.setSearchOpen(false);
}

/** The query survives close/open within the session (find-bar memory). */
export function searchQuery(): string {
  return query;
}

export function setQuery(q: string): void {
  query = q;
  current = 0;
  if (!useFlux.getState().searchOpen) return;
  scan(true); // a NEW query scrolls to its first hit, like native find
}

export function nextMatch(): void {
  if (ranges.length === 0) return;
  current = (current + 1) % ranges.length;
  publish();
  registerHighlights();
  scrollToCurrent();
}

export function prevMatch(): void {
  if (ranges.length === 0) return;
  current = (current - 1 + ranges.length) % ranges.length;
  publish();
  registerHighlights();
  scrollToCurrent();
}

// ── Scan ────────────────────────────────────────────────────────────────

function scan(scroll: boolean): void {
  const cid = useFlux.getState().activeChatId;
  const pane = cid ? getPaneIfExists(cid) : undefined;
  if (!pane || !query) {
    ranges = [];
    current = 0;
    registerHighlights();
    publish();
    return;
  }
  // The pane may have been REPLACED under us (stale resync wipe, LRU
  // eviction + revisit) — re-point the observer before rescanning.
  if (pane !== observedPane) attach(pane);
  ranges = scanPane(pane, query);
  if (current >= ranges.length) current = 0;
  registerHighlights();
  publish();
  if (scroll && ranges.length > 0) scrollToCurrent();
}

/** Join the pane's text nodes (skipping the empty-state card) into one
 * lowercase haystack, find non-overlapping hits, and map each back to a
 * Range over the original nodes (cross-node spans included). */
function scanPane(pane: HTMLDivElement, needleRaw: string): Range[] {
  const needle = needleRaw.toLowerCase();
  if (!needle) return [];
  const nodes: Text[] = [];
  const walker = document.createTreeWalker(pane, NodeFilter.SHOW_TEXT, {
    acceptNode(node: Text): number {
      // The empty-state card is invitation copy, not transcript content —
      // searching it would scroll matches into an empty chat.
      if (node.parentElement?.closest('.fx-empty-state')) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  for (let n = walker.nextNode() as Text | null; n; n = walker.nextNode() as Text | null) {
    nodes.push(n);
  }
  if (nodes.length === 0) return [];

  const offsets = new Array<number>(nodes.length);
  let total = 0;
  for (let i = 0; i < nodes.length; i++) {
    offsets[i] = total;
    total += nodes[i].data.length;
  }
  const haystack = nodes.map((n) => n.data).join('').toLowerCase();

  // Monotonic cursor: matches are found in order, so node lookup for both
  // ends advances forward only — O(nodes) across the whole scan.
  let cursor = 0;
  const locate = (abs: number): number => {
    while (cursor + 1 < nodes.length && offsets[cursor + 1] <= abs) cursor++;
    return cursor;
  };

  const found: Range[] = [];
  let idx = haystack.indexOf(needle);
  while (idx !== -1) {
    const end = idx + needle.length;
    const s = locate(idx);
    const e = locate(end);
    const range = document.createRange();
    range.setStart(nodes[s], idx - offsets[s]);
    range.setEnd(nodes[e], end - offsets[e]);
    found.push(range);
    idx = haystack.indexOf(needle, end);
  }
  return found;
}

function registerHighlights(): void {
  if (!searchSupported()) return;
  CSS.highlights.delete(NAME_ALL);
  CSS.highlights.delete(NAME_CURRENT);
  if (ranges.length === 0) return;
  const all = new Highlight();
  for (const r of ranges) all.add(r);
  CSS.highlights.set(NAME_ALL, all);
  const cur = new Highlight();
  cur.add(ranges[current]);
  CSS.highlights.set(NAME_CURRENT, cur);
}

function publish(): void {
  useFlux.setState({ searchMatches: ranges.length, searchCurrent: current });
}

function scrollToCurrent(): void {
  const el = ranges[current]?.startContainer.parentElement;
  // Optional call: jsdom has no scrollIntoView (unit tests stub it to spy).
  el?.scrollIntoView?.({ block: 'center' });
}

// ── Pane mutation → debounced rescan ────────────────────────────────────

function attach(pane: HTMLDivElement): void {
  detach();
  observedPane = pane;
  observer = new MutationObserver(() => {
    if (rescanTimer !== null) clearTimeout(rescanTimer);
    rescanTimer = window.setTimeout(() => {
      rescanTimer = null;
      // Streaming append: re-count WITHOUT scrolling — the view must stay
      // where the reader put it (the stick-state follow owns repositioning).
      if (useFlux.getState().searchOpen) scan(false);
    }, RESCAN_DEBOUNCE_MS);
  });
  observer.observe(pane, { childList: true, characterData: true, subtree: true });
}

function detach(): void {
  observer?.disconnect();
  observer = null;
  observedPane = null;
  if (rescanTimer !== null) {
    clearTimeout(rescanTimer);
    rescanTimer = null;
  }
}

/** Test isolation — the module state outlives a wiped DOM. */
export function _resetSearchForTest(): void {
  detach();
  query = '';
  ranges = [];
  current = 0;
  openedCid = '';
}
