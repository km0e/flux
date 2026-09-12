/**
 * dom.ts — Shared DOM construction helpers for message bubbles, tool cards,
 * reasoning blocks, and utility widgets.
 *
 * Pure DOM creation — no side effects, no dependency on state/panes/stream.
 * Callers append elements to their pane and wire scroll/state themselves.
 *
 * Provides: createCopyButton, createMessageBubble, createReasoningBlock,
 *           createToolCard, setToolCardResult, markToolCardComplete,
 *           createErrorBubble, isNearBottom, scrollPaneToBottom,
 *           alwaysScrollToBottom, forceFollow, scheduleFollow,
 *           followExpansion, followExpansionFrom
 * Depends: lib/markdown.ts
 */

import { escapeHtml } from './markdown';
import { copyText } from './clipboard';

// ── Tool icons (monochrome inline SVG, stroke follows currentColor) ─────────────────────

const TOOL_ICON_PATHS: Record<string, string> = {
  // feather: file-text — read_file / list_directory / buf_read and the fallback
  file: '<path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/>',
  // feather: edit-3 —— edit_file / write
  pencil: '<path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/>',
  // feather: terminal —— bash / shell
  terminal: '<polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/>',
  // feather: search —— grep / glob / find
  search: '<circle cx="11" cy="11" r="8"/><line x1="21" y1="21" x2="16.65" y2="16.65"/>',
  // feather: package — rust_* project tools
  box: '<path d="M21 16V8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16z"/><polyline points="3.27 6.96 12 12.01 20.73 6.96"/><line x1="12" y1="22.08" x2="12" y2="12"/>',
  // feather: share-2 — external MCP tools
  share:
    '<circle cx="18" cy="5" r="3"/><circle cx="6" cy="12" r="3"/><circle cx="18" cy="19" r="3"/><line x1="8.59" y1="13.51" x2="15.42" y2="17.49"/><line x1="15.41" y1="6.51" x2="8.59" y2="10.49"/>',
};

function toolIconKey(name: string): string {
  const n = name.toLowerCase();
  if (n === 'bash' || n.includes('shell') || n.includes('cmd')) return 'terminal';
  if (n.startsWith('edit') || n.includes('write')) return 'pencil';
  if (n.includes('grep') || n.includes('glob') || n.includes('search') || n.includes('find'))
    return 'search';
  if (n.includes('rust')) return 'box';
  if (n.includes('mcp') || n.includes('remote')) return 'share';
  return 'file';
}

/** Monochrome tool icon SVG (feather style, 24 viewBox, stroke follows currentColor). */
export function toolIconSvg(name: string): string {
  const key = toolIconKey(name);
  return (
    '<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" ' +
    'stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    (TOOL_ICON_PATHS[key] ?? TOOL_ICON_PATHS.file) +
    '</svg>'
  );
}

// ── Tool argument summary (visible in the collapsed row, no expanding needed) ───────────────

const SUMMARY_FIELD_BY_TOOL: Record<string, string> = {
  bash: 'command',
  read_file: 'path',
  edit_file: 'path',
  list_directory: 'path',
  grep: 'pattern',
  glob: 'pattern',
  buf_read: 'ref',
};

/** Extract the collapsed one-line summary from a tool name + JSON args
 * (bash command / file path / search pattern); non-JSON args fall back to the
 * raw text; truncated past 60 chars. */
export function toolSummary(name: string, args?: string): string {
  if (!args) return '';
  const raw = args.trim();
  let value = raw;
  try {
    const parsed = JSON.parse(raw) as Record<string, unknown>;
    const key = SUMMARY_FIELD_BY_TOOL[name.toLowerCase()];
    const picked =
      (key && typeof parsed[key] === 'string' ? (parsed[key] as string) : undefined) ??
      Object.values(parsed).find((v): v is string => typeof v === 'string');
    value = picked ?? raw;
  } catch {
    /* not JSON: keep the raw text */
  }
  // bash commands take the first line, so multi-line scripts cannot break the one-line summary
  value = value.split('\n')[0].replace(/\s+/g, ' ').trim();
  return value.length > 60 ? value.slice(0, 59) + '…' : value;
}

// ── Copy button ───────────────────────────────────────────────────────────

export function createCopyButton(getText: () => string): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.className = 'copy-btn';
  btn.title = 'Copy message';
  btn.textContent = 'Copy';
  btn.onclick = () => {
    // copyText, NOT navigator.clipboard directly — the Clipboard API is
    // secure-context-only; plain-HTTP LAN deployments need the fallback.
    void copyText(getText()).then((ok) => {
      if (!ok) return;
      btn.textContent = 'Copied!';
      setTimeout(() => {
        btn.textContent = 'Copy';
      }, 1500);
    });
  };
  return btn;
}

// ── Message bubbles ──────────────────────────────────────────────────────

export interface BubbleOptions {
  role: 'user' | 'assistant';
  text?: string;
  html?: string;
  raw?: string;
  /** Live raw provider — read by the copy button at click time WITHOUT
   * materializing the whole buffer into a DOM attribute per delta (the
   * streaming path accumulates in the StreamController instead). */
  rawGetter?: () => string;
  live?: boolean;
  staggerIndex?: number;
  /** History row id on a USER bubble — renders the context-rebase
   * affordance (hover reveal; always visible on touch). The click
   * behavior (confirm + bridge send) is attached by history.ts; the
   * caller only passes the id when the gates pass (classic chat, lease
   * held, id present). */
  historyId?: number;
}

export interface BubbleResult {
  el: HTMLDivElement;
  body: HTMLDivElement;
}

/** Rebase affordance icon (rotate-ccw — rewind the context). Inline SVG
 * keeps the imperative DOM self-contained (no asset fetch). */
const REBASE_SVG =
  '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
  '<path d="M2.5 8a5.5 5.5 0 1 0 1.6-3.9"/><path d="M2.5 2.5v2.6h2.6"/>' +
  '</svg>';

/**
 * Apply the shared stagger animation to an element: pushes the `staggered`
 * class and sets the `--stagger` delay from the index (capped at 12).
 */
function applyStagger(el: HTMLElement, index: number): void {
  el.classList.add('staggered');
  el.style.setProperty('--stagger', `${Math.min(index, 12) * 0.04}s`);
}

export function createMessageBubble(opts: BubbleOptions): BubbleResult {
  const div = document.createElement('div');
  const classList = ['message', opts.role];
  if (opts.live) classList.push('live');
  div.className = classList.join(' ');
  if (opts.staggerIndex !== undefined && opts.staggerIndex >= 0) {
    applyStagger(div, opts.staggerIndex);
  }

  const body = document.createElement('div');
  body.className = 'message-body prose';

  // Assistant bubbles get a header with a copy button; user bubbles don't.
  if (opts.role === 'user') {
    body.textContent = opts.text || '';
    div.appendChild(body);
    if (opts.historyId !== undefined) {
      // Rebase affordance: archive this message (inclusive) and everything
      // before it out of the model's context. Behavior in history.ts.
      const btn = document.createElement('button');
      btn.type = 'button';
      btn.className = 'msg-rebase';
      btn.dataset.baseMessageId = String(opts.historyId);
      btn.title = 'Archive context up to here';
      btn.setAttribute('aria-label', `Archive context up to message ${opts.historyId}`);
      btn.innerHTML = REBASE_SVG;
      div.insertBefore(btn, body);
    }
  } else {
    // Assistant: header + body + optional raw
    if (opts.raw !== undefined) {
      body.dataset.raw = opts.raw;
    }
    if (opts.html) {
      body.innerHTML = opts.html;
    }
    div.appendChild(body);

    const header = document.createElement('div');
    header.className = 'message-header';
    header.appendChild(
      createCopyButton(() =>
        opts.rawGetter ? opts.rawGetter() : (body.dataset.raw || body.textContent || ''),
      ),
    );
    div.insertBefore(header, body);
  }

  return { el: div, body };
}

// ── Reasoning block ──────────────────────────────────────────────────────

export interface ReasoningOptions {
  summary?: string;
  html?: string;
  /** Animated ellipsis on the summary — LIVE thinking only. Static renders
   * (history/viewer) must omit it or the dots animation plays forever. */
  dots?: boolean;
  /** Optional entry delay index — adds the 'staggered' class + --stagger delay. */
  staggerIndex?: number;
}

export interface ReasoningResult {
  el: HTMLDetailsElement;
  content: HTMLDivElement;
}

export function createReasoningBlock(opts: ReasoningOptions = {}): ReasoningResult {
  const details = document.createElement('details');
  const classList = ['message', 'thinking'];
  details.className = classList.join(' ');
  if (opts.staggerIndex !== undefined && opts.staggerIndex >= 0) {
    applyStagger(details, opts.staggerIndex);
  }

  const summary = document.createElement('summary');
  // Dots mode renders the animated ellipsis span — the literal text must
  // NOT carry its own '...' or the summary shows six dots (three static
  // beside three animated).
  summary.textContent = opts.summary ?? (opts.dots ? 'Thinking' : 'Thinking...');
  if (opts.dots) {
    const dots = document.createElement('span');
    dots.className = 'dots';
    summary.appendChild(dots);
  }
  details.appendChild(summary);

  // User-initiated expansion grows content below the fold when the block
  // sits near the pane bottom — follow it like streamed content (the
  // programmatic open in the live stream path rides the same follow via
  // the delta renders; the listener only adds the user-toggle case).
  details.addEventListener('toggle', () => {
    if (details.open) followExpansionFrom(details);
  });

  const content = document.createElement('div');
  content.className = 'thinking-content prose';
  if (opts.html) {
    content.innerHTML = opts.html;
  }
  details.appendChild(content);

  return { el: details, content };
}

// ── Tool cards ───────────────────────────────────────────────────────────

export interface ToolCardOptions {
  id: string;
  name: string;
  args?: string;
  /** 'pending' = the model is still forming the call (tool_call_preview);
   * upgraded in place to 'running' when the real tool_start lands. */
  status: 'running' | 'done' | 'pending';
  /** Optional entry delay index — adds the 'staggered' class + --stagger delay. */
  staggerIndex?: number;
}

// Elapsed-time timer registry: a running tool card refreshes its elapsed
// time every 200ms; the tick self-checks isConnected, so a removed element
// (pane cleanup) stops its timer — no leaks.
const elapsedTimers = new WeakMap<
  HTMLElement,
  { timer: ReturnType<typeof setInterval>; start: number }
>();

function formatElapsed(ms: number): string {
  const s = ms / 1000;
  if (s < 10) return s.toFixed(1) + 's';
  if (s < 60) return Math.round(s) + 's';
  return Math.floor(s / 60) + 'm ' + String(Math.round(s % 60)).padStart(2, '0') + 's';
}

function startElapsed(el: HTMLElement): void {
  const label = el.querySelector('.tool-elapsed');
  if (!label) return;
  const start = Date.now();
  const timer = setInterval(() => {
    if (!el.isConnected) {
      clearInterval(timer);
      elapsedTimers.delete(el);
      return;
    }
    label.textContent = formatElapsed(Date.now() - start);
  }, 200);
  elapsedTimers.set(el, { timer, start });
}

/** Stop the clock and return the final elapsed time ('' when not timing). */
function stopElapsed(el: HTMLElement): string {
  const entry = elapsedTimers.get(el);
  if (!entry) return '';
  clearInterval(entry.timer);
  elapsedTimers.delete(el);
  return formatElapsed(Date.now() - entry.start);
}

export function createToolCard(opts: ToolCardOptions): HTMLDivElement {
  const el = document.createElement('div');
  const classList = ['message', 'tool', opts.status];
  el.className = classList.join(' ');
  if (opts.staggerIndex !== undefined && opts.staggerIndex >= 0) {
    applyStagger(el, opts.staggerIndex);
  }
  el.dataset.toolCallId = opts.id;

  const summary = toolSummary(opts.name, opts.args);

  const statusLabel =
    opts.status === 'running'
      ? '<span class="spinner" aria-hidden="true"></span>running <span class="tool-elapsed">0.0s</span>'
      : opts.status === 'pending'
        ? '<span class="spinner" aria-hidden="true"></span>preparing'
        : ' completed';

  const header = document.createElement('div');
  header.className = 'tool-header';
  header.innerHTML =
    '<span class="tool-icon">' +
    toolIconSvg(opts.name) +
    '</span>' +
    '<strong>' +
    escapeHtml(opts.name) +
    '</strong>' +
    // flex-1 spacer: keeps the status right-aligned even without a summary
    '<span class="tool-args-summary">' +
    escapeHtml(summary) +
    '</span>' +
    '<span class="tool-status ' +
    opts.status +
    '">' +
    statusLabel +
    '</span>';
  header.setAttribute('tabindex', '0');
  header.setAttribute('role', 'button');
  header.setAttribute('aria-expanded', 'false');
  header.setAttribute('aria-label', `Toggle tool: ${opts.name}`);
  const toggleExpanded = () => {
    const expanded = !el.classList.contains('expanded');
    el.classList.toggle('expanded', expanded);
    header.setAttribute('aria-expanded', String(expanded));
    // Expansion grows the pane's content (a 180ms grid-rows transition);
    // when the card sits at the bottom the growth lands below the fold —
    // follow it like streamed content. Collapse shrinks: no follow.
    if (expanded) followExpansionFrom(el);
  };
  header.addEventListener('click', toggleExpanded);
  header.addEventListener('keydown', (e: KeyboardEvent) => {
    if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      toggleExpanded();
    }
  });
  el.appendChild(header);

  const detail = document.createElement('div');
  detail.className = 'tool-detail';

  const inner = document.createElement('div');
  inner.className = 'tool-detail-inner';

  if (opts.args) {
    const argsDiv = document.createElement('div');
    argsDiv.className = 'tool-args-display';
    argsDiv.innerHTML = '<code>' + escapeHtml(opts.args) + '</code>';
    inner.appendChild(argsDiv);
  }

  const resultContainer = document.createElement('div');
  resultContainer.className = 'tool-result-container';
  inner.appendChild(resultContainer);

  detail.appendChild(inner);
  el.appendChild(detail);

  if (opts.status === 'running') startElapsed(el);
  if (opts.status === 'pending') {
    // Live argument tail: a single text node updated per preview delta
    // (O(delta) — the full raw accumulates in dataset.rawArgs, only the
    // tail slice renders). Removed when the card upgrades to running.
    const live = document.createElement('code');
    live.className = 'tool-args-live';
    live.textContent = '';
    inner.appendChild(live);
  }

  return el;
}

export function setToolCardResult(el: HTMLElement, result: string): void {
  const resultContainer = el.querySelector('.tool-result-container');
  if (!resultContainer) return;

  resultContainer.innerHTML = '';

  if (result) {
    // Long-result size hint (only past 5 lines; hints that it expands)
    const lines = result.split('\n').length;
    if (lines > 5) {
      const meta = document.createElement('div');
      meta.className = 'tool-result-meta';
      meta.textContent = `${lines} lines`;
      resultContainer.appendChild(meta);
    }

    const pre = document.createElement('pre');
    pre.textContent = result;
    resultContainer.appendChild(pre);

    const copyBtn = createCopyButton(() => result);
    // Margin rides stream.css (.tool-result-container .copy-btn) — the
    // imperative DOM must not carry Tailwind utilities (D-25 layering).
    resultContainer.appendChild(copyBtn);
  }
}

export function markToolCardComplete(el: HTMLElement, result: string): void {
  el.classList.remove('running');
  el.classList.add('done', 'just-completed');

  const statusEl = el.querySelector('.tool-status');
  if (statusEl) {
    const elapsed = stopElapsed(el);
    statusEl.className = 'tool-status completed';
    statusEl.textContent = ' completed' + (elapsed ? ` · ${elapsed}` : '');
  }

  setToolCardResult(el, result);

  // Remove one-shot pop animation class after it plays
  el.addEventListener(
    'animationend',
    () => {
      el.classList.remove('just-completed');
    },
    { once: true },
  );
}

// ── Error and utility ────────────────────────────────────────────────────

export function createErrorBubble(message: string): HTMLDivElement {
  const el = document.createElement('div');
  el.className = 'message error';
  const icon = document.createElement('span');
  icon.className = 'error-icon';
  icon.setAttribute('aria-hidden', 'true');
  icon.textContent = '⚠';
  el.appendChild(icon);
  el.appendChild(document.createTextNode(message));
  return el;
}

/** TTFT feedback: a three-dot pulse until the first token arrives (the stream layer removes it). */
export function createTypingIndicator(): HTMLDivElement {
  const el = document.createElement('div');
  el.className = 'message typing';
  el.id = 'typing-indicator';
  el.setAttribute('aria-hidden', 'true');
  for (let i = 0; i < 3; i++) el.appendChild(document.createElement('span'));
  return el;
}

/** Neutral non-error notice (e.g. "cancelled") — distinct from error styling. */
export function createNoticeBubble(message: string): HTMLDivElement {
  const el = document.createElement('div');
  el.className = 'message notice';
  el.textContent = message;
  return el;
}

/**
 * Escape an arbitrary string for interpolation into a CSS attribute
 * selector. Uses the platform CSS.escape when available, with a minimal
 * fallback for the characters that break attribute selectors.
 */
export function escapeCssSelector(value: string): string {
  const platformEscape = (globalThis as { CSS?: { escape?: (s: string) => string } }).CSS?.escape;
  if (platformEscape) return platformEscape(value);
  return value.replace(/["\\]/g, (c) => '\\' + c);
}

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

// ── Stick-to-bottom follow (rAF merge + a stick state machine) ──
//
// Scroll-following for streamed content is NOT "force-scroll on every delta"
// but a stick state machine:
//   - attached: leaving the bottom by 8px detaches, and detaching cancels the
//     pending follow frame (a user scrolling up to read is never yanked back)
//   - detached: scrolling up stays detached; scrolling down into the bottom
//     96px re-attaches automatically
// Follow scrolls merge through requestAnimationFrame — N deltas in one frame
// scroll once, with behavior equivalent to instant (smooth compounds latency
// into wobble).

const SCROLL_ENTER = 96;

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
          // While attached, a scroll event has only two sources: the follow
          // scroll itself (programmatic, including the reposition after a burst
          // of content growth) or the user scrolling DOWN — neither is a detach
          // intent. The only detach signal is the user scrolling UP. Position-
          // based checks misjudge after a burst (stale scrollTop vs the grown
          // scrollHeight) and lose the follow.
          if (goingUp) {
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

// ── Expansion follow (user-opened tool cards / reasoning blocks) ──
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
