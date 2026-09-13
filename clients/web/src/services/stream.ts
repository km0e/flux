/**
 * stream.ts — Per-chat StreamController: single owner of all stream state.
 *
 * Provides: StreamController, getController, disposeController
 * Depends: core/state.ts, lib/markdown.ts, services/panes.ts,
 *          lib/dom.ts, lib/render.ts
 *
 * Reception and rendering are decoupled by exactly one animation frame
 * (P0-1): each delta only appends to the raw buffer and schedules a
 * coalesced re-render — at most ONE incremental render runs per frame no
 * matter how many deltas arrive. Without this, a burst of deltas queues
 * synchronous renders past the frame budget; the main thread drops frames
 * and the backlog paints in one visible surge ("chunks suddenly appear").
 * Segment boundaries (tool cards, finalize, dispose) flush synchronously —
 * a closing segment must have its final state in the DOM before the next
 * one starts.
 *
 * Committed paragraphs render once each and are appended to the DOM
 * append-only (P0-2): paragraph commits never rebuild the committed
 * region, so earlier code blocks are never re-highlighted and committed
 * nodes keep their DOM identity.
 */

import { useFlux } from '../core/state';
import { renderMarkdown } from '../lib/markdown';
import { ensurePane, getPaneIfExists } from './panes';
import {
  createMessageBubble,
  createReasoningBlock,
  createToolCard,
  createErrorBubble,
  createNoticeBubble,
  escapeCssSelector,
  scheduleFollow,
} from '../lib/dom';
import { ParagraphSplitter, renderIncremental, type FenceCache } from '../lib/render';
import { log } from '../logger';

// ── Module-level controller registry ──

/** A live reasoning block's DOM handles (registry entry; the type moved
 * here from core/types — it is stream-path plumbing, not app state). */
export interface ReasoningState {
  el: HTMLDetailsElement;
  content: HTMLDivElement;
}

const controllers = new Map<string, StreamController>();

/** Live reasoning blocks keyed by chat. DOM references live HERE, not in
 * the zustand store — the store is app state, not an element registry, and
 * every consumer of a reasoning block is in this module's render path. */
const reasoningBlocks = new Map<string, ReasoningState>();

/** Accessor for tests (the registry is module-private otherwise). */
export function getReasoningEntry(chatId: string): ReasoningState | undefined {
  return reasoningBlocks.get(chatId);
}

/** Test-only registry seeding + reset (mirrors _resetPanesForTest). */
export function _setReasoningEntryForTest(chatId: string, entry: ReasoningState): void {
  reasoningBlocks.set(chatId, entry);
}

export function _resetReasoningForTest(): void {
  reasoningBlocks.clear();
}

export function getController(chatId: string): StreamController {
  let ctrl = controllers.get(chatId);
  if (!ctrl) {
    ctrl = new StreamController(chatId);
    controllers.set(chatId, ctrl);
  }
  return ctrl;
}

export function disposeController(chatId: string): void {
  const ctrl = controllers.get(chatId);
  if (ctrl) {
    ctrl.dispose();
    controllers.delete(chatId);
  }
}

// ── Constants ──

/** Dispose idle controllers after 30s of inactivity. */
const IDLE_DISPOSE_MS = 30_000;

// ── StreamController ──

export class StreamController {
  readonly chatId: string;

  renderType = ''; // 'text' | 'reasoning' | 'tool'
  /** Whether the current text segment is live — false = next appendText starts a fresh bubble. */
  private streamActive = false;

  /** Prefix HTML cache for the unclosed code block (shared across render frames; avoids per-frame O(n²) re-renders). */
  private fenceCache: FenceCache = { key: '', html: '' };

  // ── Committed-paragraph caches (rendered once, stable DOM) ──

  private renderCache: string[] = [];
  private reasoningCache: string[] = [];

  // ── Incremental paragraph split state (P1-1: O(delta) per append) ──

  private textSplitter = new ParagraphSplitter();
  private reasoningSplitter = new ParagraphSplitter();

  // ── Raw accumulation (JS strings — V8 cons-strings make `+=` amortized
  // O(delta)). DOM attributes are written ONCE per segment finalize; per-
  // delta attribute writes would flatten + re-serialize the whole buffer
  // every delta (O(n²) per round). The copy button reads the live buffer
  // through `rawGetter`, so mid-stream copies stay exact. ──

  private rawText = '';
  private rawReasoning = '';

  // ── rAF-coalesced rendering (P0-1) ──

  /** Pending animation frame — null when no re-render is scheduled. */
  private renderHandle: number | null = null;
  private textDirty = false;
  private reasoningDirty = false;

  // ── Change-gated last-written tail HTML ──

  private lastTail = '';
  private lastReasoningTail = '';

  // ── Current streaming message elements ──

  streamDiv: HTMLDivElement | null = null;
  /** Assistant body element. `dataset.raw` lands once per segment finalize
   * (the live accumulation lives in `rawText`); announcements stream from
   * the deltas themselves. */
  streamBody: HTMLDivElement | null = null;
  private partsEl: HTMLDivElement | null = null;
  private tailEl: HTMLDivElement | null = null;

  /** Whether a reasoning block was ever created in the current text segment.
   * Gates the `closeReasoning()` call in `appendText`: a pure text stream has
   * no reasoning block to close, so we skip the redundant state lookups. */
  private reasoningEver = false;

  // ── Idle timer ──

  private idleTimer: ReturnType<typeof setTimeout> | null = null;

  /** Start time of the current reasoning segment (feeds the "Thought for Ns" summary); 0 = none in progress/exhausted. */
  private reasoningStartedAt = 0;

  /** Remove the TTFT typing indicator on first content / round wrap-up (idempotent). */
  private removeTypingIndicator(): void {
    getPaneIfExists(this.chatId)?.querySelector('#typing-indicator')?.remove();
  }

  constructor(chatId: string) {
    this.chatId = chatId;
    const checkIdle = () => {
      if (controllers.get(this.chatId) !== this) return; // superseded or already disposed
      if (this.renderType !== '') {
        // An active render longer than IDLE_DISPOSE_MS must not defeat
        // collection: re-arm and check again once the segment finalizes.
        this.idleTimer = setTimeout(checkIdle, IDLE_DISPOSE_MS);
        return;
      }
      disposeController(this.chatId);
    };
    this.idleTimer = setTimeout(checkIdle, IDLE_DISPOSE_MS);
  }

  // ── Streaming helpers ──

  /** Get or create the streaming assistant message element. */
  ensureStream(): { el: HTMLDivElement; body: HTMLDivElement } {
    if (this.streamDiv && this.streamBody && this.partsEl && this.tailEl) {
      return { el: this.streamDiv, body: this.streamBody };
    }

    const pane = ensurePane(this.chatId);
    log.debug('ensureStream: fresh bubble');
    this.removeTypingIndicator();

    const bubble = createMessageBubble({
      role: 'assistant',
      raw: '',
      live: true,
      rawGetter: () => this.rawText,
    });
    this.streamDiv = bubble.el;
    this.streamBody = bubble.body;

    // Two stable regions inside the body — committed paragraphs and the
    // live tail are updated independently on each delta.
    this.partsEl = document.createElement('div');
    this.partsEl.className = 'stream-parts';
    this.tailEl = document.createElement('div');
    this.tailEl.className = 'stream-tail';
    this.streamBody.appendChild(this.partsEl);
    this.streamBody.appendChild(this.tailEl);

    pane.appendChild(this.streamDiv);

    this.scrollToBottom();
    return { el: this.streamDiv, body: this.streamBody };
  }

  /** Get or create the reasoning block. */
  ensureReasoning(): { el: HTMLDetailsElement; content: HTMLDivElement } {
    const existing = reasoningBlocks.get(this.chatId);
    if (existing) return { el: existing.el, content: existing.content };

    const pane = ensurePane(this.chatId);
    log.debug('ensureReasoning: fresh block');
    this.removeTypingIndicator();

    // Fresh reasoning segment: the previous segment's render state describes
    // a DIFFERENT raw — stale cache/splitter entries would render
    // old-segment paragraphs into the new block (the thinking→tool→thinking
    // interleave bug).
    this.reasoningCache = [];
    this.reasoningSplitter.reset();
    this.lastReasoningTail = '';

    const block = createReasoningBlock({ dots: true });
    const details = block.el;
    const content = block.content;

    // Two stable sub-regions, mirroring the text path — committed reasoning
    // paragraphs are never re-parsed on later deltas.
    const parts = document.createElement('div');
    parts.className = 'stream-parts';
    const tail = document.createElement('div');
    tail.className = 'stream-tail';
    content.appendChild(parts);
    content.appendChild(tail);

    pane.appendChild(details);

    reasoningBlocks.set(this.chatId, { el: details, content });
    this.scrollToBottom();
    return { el: details, content };
  }

  /**
   * Finalize the reasoning block. The DOM is already fully rendered on
   * every delta (no typewriter), so this is purely closing the state entry.
   */
  closeReasoning(): void {
    this.finalizeReasoning();
  }

  private finalizeReasoning(): void {
    this.renderType = ''; // segment finished — the idle-dispose guard may run again
    const rs = reasoningBlocks.get(this.chatId);
    if (!rs) return;
    const summary = rs.el.querySelector('summary');
    if (summary) {
      // Thinking duration: <1s softens to "a moment", otherwise whole seconds
      summary.textContent = this.reasoningStartedAt
        ? `Thought for ${Math.max(1, Math.round((Date.now() - this.reasoningStartedAt) / 1000))}s`
        : 'Thought for a bit';
    }
    this.reasoningStartedAt = 0; // the next segment times afresh
    reasoningBlocks.delete(this.chatId);
  }

  // ── Scroll ──

  scrollToBottom(): void {
    const cid = useFlux.getState().activeChatId;
    if (cid !== this.chatId) return;
    // Render paths (including the dispose-time sync flush) must never throw
    // on a detached/gone pane — use the optional lookup, never create.
    const pane = getPaneIfExists(this.chatId);
    if (!pane) return;
    // rAF merge + the stick gate (dom.ts scheduleFollow): at most one scroll
    // step per frame; deltas no longer yank the user back down while reading.
    scheduleFollow(pane);
    // Button visibility is NOT cleared here — while the user reads above
    // (detached, no programmatic scroll, no scroll events) clearing per delta
    // would flicker the button away. Convergence belongs to two places: the
    // scroll event triggered by a follow scroll (the pane listener recomputes
    // → at bottom → hide) and setStreaming.
  }

  // ── Streaming append methods ──

  /** Fold a backend text delta into the raw buffer and schedule a render. */
  appendText(delta: string): void {
    // An empty delta carries no content — creating a bubble for it would
    // leave an empty assistant bubble behind after stream_end.
    if (!delta) return;
    log.debug('appendText delta=' + delta.length + ' active=' + this.streamActive);
    // Reasoning ran inside this text segment (text → thinking → text interleave):
    // close the block AND start a fresh text bubble. The reasoning block sits
    // AFTER the current bubble in the pane, so continuing into the same bubble
    // would render later text above the thinking — breaking timeline order.
    // The earlier bubble is kept (nothing is discarded). This is a segment
    // boundary: pending renders must land synchronously first — the reset
    // below drops the old element refs, and a deferred callback would find
    // nothing to render into (the first bubble's tail would be lost).
    if (this.reasoningEver) {
      this.flushRenderNow();
      this.closeReasoning();
      this.streamActive = false;
    }

    if (!this.streamActive) {
      log.debug('appendText: resetStreamDom (new segment)');
      this.resetStreamDom();
    }
    this.streamActive = true;
    this.renderType = 'text';

    this.ensureStream();
    this.rawText += delta;
    this.textSplitter.push(delta);
    useFlux.getState().setStreaming(this.chatId, true);
    this.scheduleRender('text');
  }

  /** Fold a backend reasoning delta into the raw buffer and schedule a render. */
  appendReasoning(delta: string): void {
    // Same guard as appendText: an empty delta must not materialize an
    // empty reasoning block.
    if (!delta) return;
    if (!this.reasoningStartedAt) this.reasoningStartedAt = Date.now();
    log.debug('appendReasoning delta=' + delta.length);
    this.reasoningEver = true;
    this.renderType = 'reasoning';
    this.ensureReasoning();
    // The block stays COLLAPSED by default — all of them, including the
    // first of a round. The summary carries the live signal (animated dots
    // while thinking, "Thought for Ns" after); the content streams into the
    // collapsed block and is one click away. (An earlier version forced the
    // first block open; the user preference is uniformly closed.)
    this.rawReasoning += delta;
    this.reasoningSplitter.push(delta);
    this.scheduleRender('reasoning');
  }

  // ── rAF-coalesced rendering (P0-1) ──

  /** Schedule a coalesced re-render: at most ONE incremental render runs
   * per animation frame regardless of how many deltas arrive. The hot
   * append path lands here; segment boundaries flush synchronously via
   * {@link flushRenderNow}. */
  private scheduleRender(kind: 'text' | 'reasoning'): void {
    if (kind === 'text') this.textDirty = true;
    else this.reasoningDirty = true;
    if (this.renderHandle !== null) return;
    this.renderHandle = requestAnimationFrame(() => {
      this.renderHandle = null;
      this.flushRenderNow();
    });
  }

  /** Render pending updates synchronously. Boundary paths (segment closes,
   * tool cards, finalize, dispose) and tests call this — a closing segment
   * must have its final state in the DOM before the next one starts. */
  flushRenderNow(): void {
    if (this.renderHandle !== null) {
      cancelAnimationFrame(this.renderHandle);
      this.renderHandle = null;
    }
    // Reasoning first: an interleave closes the reasoning block when the
    // text delta arrives, so its final render must land while the block
    // still exists (renderReasoning reads the live state entry).
    if (this.reasoningDirty) {
      this.reasoningDirty = false;
      this.renderReasoning();
    }
    if (this.textDirty) {
      this.textDirty = false;
      this.renderText();
    }
  }

  /**
   * Incrementally render the entire current raw buffer at full length and
   * sync the parts/tail regions. Runs at most once per animation frame —
   * the DOM trails reception by ≤1 frame.
   */
  private renderText(): void {
    if (!this.streamBody || !this.partsEl || !this.tailEl) return;
    const { committed, tailHtml } = renderIncremental(
      this.textSplitter.getParts(),
      this.textSplitter.getTail(),
      this.renderCache,
      renderMarkdown,
      this.fenceCache,
    );
    this.syncParts(this.partsEl, this.renderCache, committed);
    if (tailHtml !== this.lastTail) {
      this.lastTail = tailHtml;
      this.tailEl.innerHTML = tailHtml;
    }
    this.scrollToBottom();
  }

  private renderReasoning(): void {
    const rs = reasoningBlocks.get(this.chatId);
    if (!rs) return;
    const { committed, tailHtml } = renderIncremental(
      this.reasoningSplitter.getParts(),
      this.reasoningSplitter.getTail(),
      this.reasoningCache,
      renderMarkdown,
      this.fenceCache,
    );
    const parts = rs.content.querySelector<HTMLDivElement>('.stream-parts');
    const tail = rs.content.querySelector<HTMLDivElement>('.stream-tail');
    if (!parts || !tail) {
      // Pre-split block (or test fixture) — assemble into the content div.
      rs.content.innerHTML = this.reasoningCache.slice(0, committed).join('\n') + '\n' + tailHtml;
      this.scrollToBottom();
      return;
    }
    this.syncParts(parts, this.reasoningCache, committed);
    if (tailHtml !== this.lastReasoningTail) {
      this.lastReasoningTail = tailHtml;
      tail.innerHTML = tailHtml;
    }
    this.scrollToBottom();
  }

  /** Append-only committed-region sync (P0-2): each committed paragraph is
   * ONE stable wrapper node — appended when it commits, removed on fence
   * fold-back, never rebuilt otherwise. A commit therefore costs O(new
   * paragraph), not O(whole message): earlier code blocks are never
   * re-highlighted and committed nodes keep their DOM identity (which also
   * keeps the `:last-child` entry animation from replaying on old parts). */
  private syncParts(partsEl: HTMLElement, cache: string[], committed: number): void {
    const children = partsEl.children;
    while (children.length > committed) {
      children[children.length - 1].remove();
    }
    for (let i = children.length; i < committed; i++) {
      const part = document.createElement('div');
      part.className = 'stream-part';
      part.dataset.idx = String(i);
      part.innerHTML = cache[i];
      partsEl.appendChild(part);
    }
  }

  /** Insert a tool-start card into the pane. A `tool_preview` pending card
   * for the same call id upgrades IN PLACE (no duplicate). */
  addToolCard(id: string, name: string, args: string): HTMLElement {
    log.debug('addToolCard ' + id + ' ' + name);
    this.flushRender(); // closes any open reasoning segment + finalizes text
    this.renderType = 'tool';
    this.streamActive = false; // tool = segment boundary; next text starts a fresh bubble

    const pane = ensurePane(this.chatId);
    this.removeTypingIndicator();

    const el = createToolCard({ id, name, args, status: 'running' });
    const pendingCard = pane.querySelector<HTMLElement>(
      `.tool.pending[data-tool-call-id="${escapeCssSelector(id)}"]`,
    );
    if (pendingCard) {
      pendingCard.replaceWith(el);
    } else {
      pane.appendChild(el);
    }
    this.scrollToBottom();
    return el;
  }

  /** A tool call the model is still forming (tool_preview): create a dimmed
   * pending card the moment its identity is known — long before its
   * arguments finish streaming and the real tool_start lands. Never
   * persisted; voided at round end when no tool_start ever upgrades it. */
  addToolPreview(id: string, name?: string): HTMLElement {
    log.debug('addToolPreview ' + id + ' ' + (name ?? ''));
    this.flushRender(); // same segment-boundary discipline as addToolCard
    this.renderType = 'tool';
    this.streamActive = false;

    const pane = ensurePane(this.chatId);
    this.removeTypingIndicator();

    // Idempotent: an identity event is emitted exactly once per call, but
    // a stale duplicate must never double-render the card.
    const existing = pane.querySelector<HTMLElement>(
      `.tool.pending[data-tool-call-id="${escapeCssSelector(id)}"]`,
    );
    if (existing) return existing;

    const el = createToolCard({ id, name: name ?? '', args: '', status: 'pending' });
    pane.appendChild(el);
    this.scrollToBottom();
    return el;
  }

  /** Append a raw argument fragment to a pending preview card's live tail.
   * The full raw accumulates in dataset.rawArgs; only the tail slice
   * renders (O(delta) per fragment, no JSON parsing of partial input). */
  appendToolPreviewArgs(id: string, delta: string): void {
    const pane = getPaneIfExists(this.chatId);
    const el = pane?.querySelector<HTMLElement>(
      `.tool.pending[data-tool-call-id="${escapeCssSelector(id)}"]`,
    );
    if (!el) return;
    const raw = (el.dataset.rawArgs ?? '') + delta;
    el.dataset.rawArgs = raw;
    const tail = el.querySelector('.tool-args-live');
    if (tail) {
      const TAIL = 200;
      tail.textContent = raw.length > TAIL ? '…' + raw.slice(-TAIL) : raw;
    }
  }

  /** Append an error message to the pane. */
  appendError(message: string): void {
    const pane = ensurePane(this.chatId);
    const el = createErrorBubble(message);
    pane.appendChild(el);
    this.scrollToBottom();
  }

  /** Append a neutral notice (e.g. "cancelled") — not an error. */
  appendNotice(message: string): void {
    const pane = ensurePane(this.chatId);
    const el = createNoticeBubble(message);
    pane.appendChild(el);
    this.scrollToBottom();
  }

  // ── Finalization ──

  /**
   * Finalize the current text segment. The DOM is already fully rendered on
   * every delta, so this only marks the segment done — a late delta from a
   * still-live round starts a fresh bubble.
   */
  flushRender(): void {
    // Land pending renders BEFORE finalizing — the segment's last deltas
    // must be in the DOM when the round wraps up (stream_end is terminal).
    this.flushRenderNow();
    // Reasoning-only rounds never create a text segment — finalize the
    // reasoning block here too, or it hangs on "Thinking..." forever.
    this.closeReasoning();
    this.finalizeRender();
  }

  private finalizeRender(): void {
    // The raw lands ONCE per segment end — copy buttons (and any attribute
    // reader) see the full markdown without per-delta serialization.
    if (this.streamBody) this.streamBody.dataset.raw = this.rawText;
    if (this.streamDiv) this.streamDiv.classList.remove('live');
    this.streamActive = false; // finalized segment — a late delta starts a fresh bubble
    this.renderType = ''; // segment finished — the idle-dispose guard may run again
    this.removeTypingIndicator();
    log.debug('finalizeRender');
  }

  // ── Lifecycle ──

  private resetStreamDom(): void {
    log.debug('resetStreamDom');
    this.streamActive = false;
    this.streamDiv = null;
    this.streamBody = null;
    this.partsEl = null;
    this.tailEl = null;
    this.reasoningEver = false;
    this.reasoningStartedAt = 0;
    this.renderCache = [];
    this.reasoningCache = [];
    this.textSplitter.reset();
    this.reasoningSplitter.reset();
    this.fenceCache = { key: '', html: '' };
    this.lastTail = '';
    this.lastReasoningTail = '';
    this.rawText = '';
    this.rawReasoning = '';
    // Defensive: no pending render may survive a reset (every reset path
    // flushes first; this keeps the invariant local even if one is missed).
    if (this.renderHandle !== null) {
      cancelAnimationFrame(this.renderHandle);
      this.renderHandle = null;
    }
    this.textDirty = false;
    this.reasoningDirty = false;
  }

  /** Full dispose: clear entries and remove from signals. */
  dispose(): void {
    if (this.idleTimer) {
      clearTimeout(this.idleTimer);
      this.idleTimer = null;
    }
    // The DOM is kept on dispose ("no content loss") — land pending renders
    // into the existing elements first, then drop the controller state.
    this.flushRenderNow();
    if (this.streamDiv) this.streamDiv.classList.remove('live');
    this.resetStreamDom();

    useFlux.getState().clearStreaming(this.chatId);
    reasoningBlocks.delete(this.chatId);
    this.removeTypingIndicator();
  }
}
