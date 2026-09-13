/**
 * dom.ts — Shared DOM construction helpers for message bubbles, tool cards,
 * reasoning blocks, and utility widgets.
 *
 * Pure DOM creation — no side effects, no dependency on state/panes/stream.
 * Callers append elements to their pane and wire scroll/state themselves.
 * The scroll-follow state machine lives in lib/follow.ts; this module
 * re-exports it so every consumer keeps a single import surface.
 *
 * Provides: createCopyButton, createMessageBubble, createReasoningBlock,
 *           createToolCard, setToolCardResult, markToolCardComplete,
 *           createErrorBubble, escapeCssSelector,
 *           re-exports from lib/follow.ts (isNearBottom, alwaysScrollToBottom,
 *           scrollPaneToBottom, forceFollow, scheduleFollow,
 *           followExpansion, followExpansionFrom)
 * Depends: lib/markdown.ts, lib/clipboard.ts, lib/follow.ts
 */

import { escapeHtml } from './markdown';
import { copyText } from './clipboard';
import { followExpansionFrom } from './follow';

export {
  isNearBottom,
  alwaysScrollToBottom,
  scrollPaneToBottom,
  forceFollow,
  scheduleFollow,
  followExpansion,
  followExpansionFrom,
} from './follow';

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
  /** The store row id on a USER bubble — renders the fork affordance
   * (hover reveal; always visible on touch). The click behavior (the
   * fork send) is attached by history.ts; the caller only passes the id
   * when it is known (persisted messages). */
  forkPoint?: number;
}

export interface BubbleResult {
  el: HTMLDivElement;
  body: HTMLDivElement;
}

const FORK_SVG =
  '<svg viewBox="0 0 16 16" width="13" height="13" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
  '<circle cx="4" cy="3.5" r="1.7"/><circle cx="4" cy="12.5" r="1.7"/><circle cx="12" cy="5.5" r="1.7"/>' +
  '<path d="M4 5.2v5.6"/><path d="M4 7c0-1.6 1.6-2.6 4-2.6 2 0 4-.4 4 1.1"/>' +
  '</svg>';

/** Build the fork affordance button (shared by the history render and the
 * live path — a bubble gains it the moment its store row id is known).
 * The row id IS the fork point: forking creates a NEW conversation holding
 * the transcript up to but excluding this message (the redo turn — its
 * content prefills the fork's composer); the source is untouched, so the
 * affordance needs no destructive-action confirmation. */
export function buildForkButton(forkPoint: number): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'msg-fork';
  btn.dataset.forkPoint = String(forkPoint);
  btn.title = 'Fork into a new conversation from this message';
  btn.setAttribute('aria-label', `Fork a new conversation from message ${forkPoint}`);
  btn.innerHTML = FORK_SVG;
  return btn;
}

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
    if (opts.forkPoint !== undefined) {
      // Fork affordance: a new conversation restarts before this message
      // (its content prefills the fork's composer). Behavior in history.ts.
      div.insertBefore(buildForkButton(opts.forkPoint), body);
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
  // sits near the pane bottom — follow it like streamed content. Blocks are
  // born collapsed everywhere (live + history); the only open transition is
  // the user's own toggle.
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
    // imperative DOM must not carry Tailwind utilities (layering).
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
