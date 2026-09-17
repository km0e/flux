/**
 * toolcard.ts — the tool-card family: the imperative DOM builders for the
 * conversation's flight records.
 *
 * Split out of dom.ts (which re-exports the public names for stability).
 * This is the LOWER of the two modules: it depends only on markdown /
 * clipboard / follow — and hosts the two presentation ATOMS both families
 * share (createCopyButton, applyStagger), so the import direction stays
 * one-way (dom.ts → toolcard.ts, never back).
 *
 * The card is lazy about its result: the string rides a WeakMap registry
 * and the <pre> materializes on the card's first expansion (most cards
 * are never opened). The verdict (classifyToolResult) reads the kernel's
 * own string markers — see the section comment below.
 *
 * Provides: toolIconSvg, toolSummary, createCopyButton, ToolCardOptions,
 *           createToolCard, ToolVerdict, classifyToolResult,
 *           setToolCardResult, markToolCardComplete, applyStagger
 * Depends: lib/markdown.ts, lib/clipboard.ts, lib/follow.ts
 */

import { escapeHtml } from './markdown';
import { copyText } from './clipboard';
import { followExpansionFrom } from './follow';

// ── Shared presentation atoms ────────────────────────────────────────────
// These live here (not in dom.ts) because BOTH the tool family and the
// bubble family use them and this module is the lower one — dom.ts imports
// them back for its bubbles/reasoning blocks.

/** The shared "Copy" button (tool results + assistant message headers).
 * Uses copyText, NOT navigator.clipboard directly — the Clipboard API is
 * secure-context-only; plain-HTTP LAN deployments need the fallback. */
export function createCopyButton(getText: () => string): HTMLButtonElement {
  const btn = document.createElement('button');
  btn.className = 'copy-btn';
  btn.title = 'Copy message';
  btn.textContent = 'Copy';
  btn.onclick = () => {
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

/**
 * Apply the shared stagger animation to an element: pushes the `staggered`
 * class and sets the `--stagger` delay from the index (capped at 12).
 */
export function applyStagger(el: HTMLElement, index: number): void {
  el.classList.add('staggered');
  el.style.setProperty('--stagger', `${Math.min(index, 12) * 0.04}s`);
}

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
  read_file: 'file_path',
  edit_file: 'file_path',
  list_directory: 'path',
  grep: 'patterns',
  glob: 'patterns',
  buf_read: 'ref',
  // Legacy names (the tools were removed in E5) — kept so historical
  // transcripts still summarize their tool cards.
  read_files: 'files',
  edit_files: 'edits',
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
    const mapped = key ? parsed[key] : undefined;
    let picked: string | undefined;
    if (typeof mapped === 'string') {
      picked = mapped;
    } else if (Array.isArray(mapped)) {
      // Legacy multi-item args + object arrays with a file_path field:
      // "N items: <first path>".
      const firstPath = (mapped[0] as Record<string, unknown> | undefined)?.file_path;
      if (typeof firstPath === 'string') {
        picked = mapped.length > 1 ? `${mapped.length} items: ${firstPath}` : firstPath;
      } else if (typeof mapped[0] === 'string' && key === 'patterns') {
        // Batched search tools (grep/glob): "N patterns: <first>". Scoped to
        // the patterns field — other tools' string arrays keep the raw
        // fallback (pinned behavior for malformed multi-item args).
        picked = mapped.length > 1 ? `${mapped.length} patterns: ${mapped[0]}` : mapped[0];
      }
    }
    value =
      picked ??
      (Object.values(parsed).find((v): v is string => typeof v === 'string') as string | undefined) ??
      raw;
  } catch {
    /* not JSON: keep the raw text */
  }
  // bash commands take the first line, so multi-line scripts cannot break the one-line summary
  value = value.split('\n')[0].replace(/\s+/g, ' ').trim();
  return value.length > 60 ? value.slice(0, 59) + '…' : value;
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
    // Lazy result materialization: the collapsed card carries NO result
    // <pre> (most cards are never opened — defer the text layout until
    // the first expand). The result string rides the registry, so the
    // copy button works collapsed too.
    if (expanded) {
      materializeToolResult(el);
      followExpansionFrom(el);
    }
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

// ── Tool results: lazy materialization ───────────────────────────────────
//
// The result string NEVER enters the DOM at set time — it rides a registry
// and the <pre> is built on the card's first expansion. Rationale: the
// server bounds every tool result at the inline budget (~8000 chars, see
// flux-chat's bounded_output), so the size is legal — but a long
// conversation carries dozens of cards whose collapsed result containers
// would still pay text layout + retained DOM for content nobody reads.
// Deferring costs nothing (the copy button reads the registry/closure, not
// the <pre>) and removes the per-card layout weight entirely.
const toolResults = new WeakMap<HTMLElement, string>();

/**
 * Build the result <pre> for a tool card from the registry — idempotent
 * (a materialized card is a no-op). Called on first expansion; empty
 * results never materialize anything.
 */
function materializeToolResult(el: HTMLElement): void {
  const result = toolResults.get(el);
  if (!result) return;
  const container = el.querySelector('.tool-result-container');
  if (!container || container.querySelector('pre')) return;
  const pre = document.createElement('pre');
  pre.textContent = result;
  // Before the copy button (which setToolCardResult appended last).
  const copyBtn = container.querySelector('.copy-btn');
  if (copyBtn) container.insertBefore(pre, copyBtn);
  else container.appendChild(pre);
}

// ── Tool verdicts: the flight's outcome voice ────────────────────────────
// The wire carries tool results as PLAIN STRINGS (no error flag), so the
// outcome is classified from the kernel's own string markers:
//   - a tool whose execute() returned Err becomes "Error: {e}"
//     (crates/flux-chat/src/chat.rs), as do crashed/panicked flights
//     (crates/flux-chat/src/tool_exec.rs);
//   - a flight the user cancelled is marked with INTERRUPTED_MARK
//     (crates/flux-core/src/types.rs) before the result is emitted;
//   - a shell command that exited non-zero carries a trailing
//     "(exit code: N)" line (crates/flux-tools format_command_output).
// Known false-positive face (accepted): a SUCCESSFUL shell command can
// echo text that begins like a marker ("Error: …"); the verdict colors
// the card's summary voice only — the result text itself is never
// rewritten and remains the truth.
const INTERRUPTED_MARK = '[interrupted by user]';
const EXIT_CODE_SUFFIX = /\(exit code: (-?\d+)\)\s*$/;

export type ToolVerdict = 'ok' | 'error' | 'interrupted' | 'exit';

export function classifyToolResult(result: string): { verdict: ToolVerdict; code?: number } {
  if (result.startsWith(INTERRUPTED_MARK)) return { verdict: 'interrupted' };
  if (result.startsWith('Error: ')) return { verdict: 'error' };
  const m = EXIT_CODE_SUFFIX.exec(result);
  if (m && m[1] !== '0') return { verdict: 'exit', code: Number(m[1]) };
  return { verdict: 'ok' };
}

const TOOL_VERDICT_CLASSES = ['tool-error', 'tool-interrupted', 'tool-exit'] as const;
const TOOL_VERDICT_LABEL: Record<Exclude<ToolVerdict, 'ok'>, string> = {
  error: ' error',
  interrupted: ' interrupted',
  exit: ' exit',
};

/** Paint the verdict onto a completed card: the card class (danger rail on
 * error, full opacity — settled cards fade, failures must not) and the
 * status span's voice. 'ok' only clears stale verdict classes; the status
 * label (' completed' / ' result') belongs to the caller. */
function applyToolVerdict(el: HTMLElement, result: string): void {
  const { verdict, code } = classifyToolResult(result);
  el.classList.remove(...TOOL_VERDICT_CLASSES);
  if (verdict === 'ok') return;
  el.classList.add(`tool-${verdict}`);
  const statusEl = el.querySelector('.tool-status');
  if (statusEl) {
    statusEl.className = `tool-status ${verdict}`;
    statusEl.textContent = verdict === 'exit' ? ` exit ${code}` : TOOL_VERDICT_LABEL[verdict];
  }
}

export function setToolCardResult(el: HTMLElement, result: string): void {
  const resultContainer = el.querySelector('.tool-result-container');
  if (!resultContainer) return;

  toolResults.set(el, result);
  resultContainer.innerHTML = '';
  applyToolVerdict(el, result);

  if (result) {
    // Long-result size hint (only past 5 lines; hints that it expands)
    const lines = result.split('\n').length;
    if (lines > 5) {
      const meta = document.createElement('div');
      meta.className = 'tool-result-meta';
      meta.textContent = `${lines} lines`;
      resultContainer.appendChild(meta);
    }

    // No <pre> here — materialized lazily on first expansion (module
    // comment). An ALREADY-expanded card must not wait for a collapse/
    // expand cycle: materialize immediately.
    if (el.classList.contains('expanded')) materializeToolResult(el);

    const copyBtn = createCopyButton(() => result);
    // Margin rides stream.css (.tool-result-container .copy-btn) — the
    // imperative DOM must not carry Tailwind utilities (layering).
    resultContainer.appendChild(copyBtn);
  } else {
    toolResults.delete(el);
  }
}

export function markToolCardComplete(el: HTMLElement, result: string): void {
  el.classList.remove('running');
  el.classList.add('done', 'just-completed');

  const elapsed = stopElapsed(el);

  setToolCardResult(el, result);

  // The verdict label is painted for abnormal results (applyToolVerdict);
  // an 'ok' result replaces whatever live voice the span carried (the
  // spinner + "running" markup) with the completed one. Every voice
  // carries the elapsed time.
  const { verdict } = classifyToolResult(result);
  const statusEl = el.querySelector('.tool-status');
  if (statusEl) {
    if (verdict === 'ok') {
      statusEl.className = 'tool-status completed';
      statusEl.textContent = ' completed';
    }
    statusEl.textContent += elapsed ? ` · ${elapsed}` : '';
  }

  // Remove one-shot pop animation class after it plays
  el.addEventListener(
    'animationend',
    () => {
      el.classList.remove('just-completed');
    },
    { once: true },
  );
}
