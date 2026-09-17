/**
 * dom.ts — the imperative DOM builders for the conversation transcript:
 * message bubbles (user/assistant + fork/copy affordances), the reasoning
 * block, the error/notice bubbles, the TTFT typing indicator, and the CSS
 * selector escape utility.
 *
 * The tool-card family lives in `toolcard.ts` (its public names are
 * re-exported here for stability); the scroll-follow machinery lives in
 * `follow.ts` (likewise re-exported). This module imports the shared
 * presentation atoms (createCopyButton, applyStagger) back from toolcard —
 * the one import direction; toolcard never imports dom.
 *
 * Provides: createMessageBubble, buildForkButton, buildMsgCopyButton,
 *           createReasoningBlock, createErrorBubble, createTypingIndicator,
 *           createNoticeBubble, escapeCssSelector; re-exports the follow
 *           family (isNearBottom … followExpansionFrom) and the toolcard
 *           family (toolIconSvg … markToolCardComplete)
 * Depends: lib/markdown.ts, lib/clipboard.ts, lib/follow.ts, lib/toolcard.ts
 */

import { copyText } from './clipboard';
import { followExpansionFrom } from './follow';
import { applyStagger, createCopyButton } from './toolcard';

export {
  isNearBottom,
  alwaysScrollToBottom,
  scrollPaneToBottom,
  forceFollow,
  scheduleFollow,
  followExpansion,
  followExpansionFrom,
} from './follow';

export {
  applyStagger,
  createCopyButton,
  toolIconSvg,
  toolSummary,
  createToolCard,
  classifyToolResult,
  setToolCardResult,
  markToolCardComplete,
} from './toolcard';
export type { ToolCardOptions, ToolVerdict } from './toolcard';

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

// feather: copy / check (24 viewBox, downscaled to the fork icon's size)
const COPY_SVG =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
  '<rect x="9" y="9" width="13" height="13" rx="2" ry="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>' +
  '</svg>';

const CHECK_SVG =
  '<svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
  '<polyline points="20 6 9 17 4 12"/>' +
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

/** Build the copy affordance button for USER bubbles (assistant bubbles
 * carry the text Copy button in their header instead). The text is read
 * at click time; success flips the icon to a check for 1.5s — failure
 * stays silent (copyText already tried the legacy fallback; a false
 * "Copied" would be worse than no feedback). */
export function buildMsgCopyButton(getText: () => string): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'msg-copy';
  btn.title = 'Copy message';
  btn.setAttribute('aria-label', 'Copy message');
  btn.innerHTML = COPY_SVG;
  let reset: ReturnType<typeof setTimeout> | undefined;
  btn.onclick = () => {
    void copyText(getText()).then((ok) => {
      if (!ok) return;
      btn.innerHTML = CHECK_SVG;
      btn.classList.add('ok');
      // A repeat click during the feedback window restarts the timer.
      clearTimeout(reset);
      reset = setTimeout(() => {
        btn.innerHTML = COPY_SVG;
        btn.classList.remove('ok');
      }, 1500);
    });
  };
  return btn;
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

  // Assistant bubbles get a header with a copy button; user bubbles get
  // an icon copy affordance beside the (optional) fork button — both sit
  // LEFT of the bubble and insert before the body, so the order is
  // canonical (copy · fork · bubble) on the history AND the live path.
  if (opts.role === 'user') {
    body.textContent = opts.text || '';
    div.appendChild(body);
    // Copy affordance: reads the bubble's own text at click time. Both
    // affordances insert BEFORE the body, so the order is canonical
    // (copy · fork · bubble) on the history AND the live-attach path.
    div.insertBefore(buildMsgCopyButton(() => opts.text || body.textContent || ''), body);
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

// ── Error and utility ────────────────────────────────────────────────────

export function createErrorBubble(
  message: string,
  opts: { retry?: boolean } = {},
): HTMLDivElement {
  const el = document.createElement('div');
  el.className = 'message error';
  const icon = document.createElement('span');
  icon.className = 'error-icon';
  icon.setAttribute('aria-hidden', 'true');
  icon.textContent = '⚠';
  el.appendChild(icon);

  const text = document.createElement('span');
  text.className = 'error-text';
  text.textContent = message;
  el.appendChild(text);

  // Pane-context errors only (a bare no-chat error has no turn to retry).
  // RETRY REFILLS, NEVER RESENDS: the failed turn's user text goes back
  // into the composer (the same flux:compose bridge the prompt
  // suggestions and fork drafts use) for review — an auto-send would
  // stack a second round on top of the failed one's context, and the
  // failed round stays in the transcript (the user splits conversations
  // manually — the accepted tradeoff, docs/decisions T-08).
  if (opts.retry) {
    const actions = document.createElement('span');
    actions.className = 'error-actions';
    actions.setAttribute('role', 'group');
    actions.setAttribute('aria-label', 'Error actions');

    const retry = document.createElement('button');
    retry.type = 'button';
    retry.className = 'error-action';
    retry.title = 'Put your last message back into the composer';
    retry.setAttribute('aria-label', 'Retry: put your last message back into the composer');
    retry.textContent = 'Retry';
    retry.onclick = () => {
      // Read at CLICK time — the bubble is in the pane by then (at build
      // time it is not attached yet). Walk back to the nearest USER
      // bubble; its body text is the exact turn that failed.
      let prev = el.previousElementSibling;
      while (prev && !prev.classList.contains('user')) {
        prev = prev.previousElementSibling;
      }
      const content = (prev?.querySelector('.message-body')?.textContent ?? '').trim();
      if (content) {
        window.dispatchEvent(new CustomEvent('flux:compose', { detail: content }));
      }
    };
    actions.appendChild(retry);

    const copy = document.createElement('button');
    copy.type = 'button';
    copy.className = 'error-action';
    copy.title = 'Copy the error message';
    copy.setAttribute('aria-label', 'Copy error message');
    copy.textContent = 'Copy';
    copy.onclick = () => {
      void copyText(message);
    };
    actions.appendChild(copy);

    el.appendChild(actions);
  }
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
