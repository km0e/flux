/**
 * render.ts — Pure markdown rendering helpers for the streaming render loop.
 *
 * No DOM access — render fn injected for testability.
 *
 * Two building blocks:
 * - {@link ParagraphSplitter}: maintains committed paragraph sources + the
 *   live tail across appends. A delta with no separator and no fence tick
 *   cannot change the structure (the fold condition only moves on a
 *   separator or a tick), so the fast path is O(delta) instead of a
 *   full-message re-split per frame.
 * - {@link renderIncremental}: renders committed paragraphs once each
 *   (cached — the caller appends their HTML to the DOM append-only) and the
 *   tail as a stable slice (fence prefix cached, open construct escaped).
 *
 * Provides: splitAndFold, ParagraphSplitter, renderIncremental,
 *           renderStableSlice, FenceCache
 * Depends: lib/markdown.ts
 */

import { escapeHtml } from './markdown';

/** Result of {@link renderIncremental}. */
export interface IncrementalResult {
  /** Number of committed paragraphs (cache.length === committed after the call). */
  committed: number;
  /** Rendered live tail (stable slice; empty when the tail is empty). */
  tailHtml: string;
}

/** Prefix HTML cache for an unclosed ``` block (shared across calls): the
 * stablePrefix is constant while the block grows incrementally — only the
 * escaped tail re-renders, the prefix renders exactly once.
 * The caller (StreamController) holds one instance per chat. */
export interface FenceCache {
  key: string;
  html: string;
}

const fenceParity = (s: string) => (s.match(/```/g) || []).length % 2;

/** Fold trailing committed parts into the tail while an open construct
 * straddles the boundary: the tail's parity is odd, or the last committed
 * part is odd — e.g. a closing ``` landing in its own paragraph after a
 * blank line inside a code block would otherwise commit as a standalone
 * part and render as an empty `<pre>`. Pure. */
function foldTrailing(parts: string[], tail: string): { parts: string[]; tail: string } {
  const p = [...parts];
  let t = tail;
  while (p.length > 0 && (fenceParity(t) !== 0 || fenceParity(p[p.length - 1]) !== 0)) {
    t = p.pop()! + '\n\n' + t;
  }
  return { parts: p, tail: t };
}

/**
 * Split raw markdown on paragraph boundaries, folding trailing paragraphs
 * back into the tail until code fences balance.
 *
 * The tail is the trailing suffix of raw (fold-back rejoins with '\n\n';
 * original separators longer than 2 newlines lose the extra newlines in the
 * folded tail — whitespace only, invisible in rendered markdown).
 */
export function splitAndFold(raw: string): { parts: string[]; tail: string } {
  const segs = raw.split(/\n\n+/);
  const tail = segs.pop() || '';
  return foldTrailing(segs, tail);
}

/**
 * Incremental paragraph splitter (P1-1): maintains the committed-part /
 * tail split across appends instead of re-splitting the whole raw buffer
 * per frame.
 *
 * Fast path — a delta with no `\n\n`, no ``` tick, and a tail that carries
 * no embedded separator cannot change the structure: the fold condition
 * (tail parity / last-part parity) only moves on a separator or a tick, so
 * the tail just grows. Everything else takes the exact `splitAndFold`
 * semantics over tail+delta (fold-back may pull committed parts back in).
 */
export class ParagraphSplitter {
  private parts: string[] = [];
  private tail = '';

  reset(): void {
    this.parts = [];
    this.tail = '';
  }

  /** Committed paragraph sources (never re-parsed after commit). */
  getParts(): string[] {
    return this.parts;
  }

  /** The live tail (re-parsed per render). */
  getTail(): string {
    return this.tail;
  }

  push(delta: string): void {
    this.tail += delta;
    if (!/```/.test(delta) && !delta.includes('\n\n') && !this.tail.includes('\n\n')) {
      return;
    }
    const segs = this.tail.split(/\n\n+/);
    const tail = segs.pop() || '';
    const merged = [...this.parts, ...segs];
    const folded = foldTrailing(merged, tail);
    this.parts = folded.parts;
    this.tail = folded.tail;
  }
}

/**
 * Render the splitter's current state: committed paragraphs render exactly
 * once each (cached — the caller appends their HTML to the DOM append-only,
 * so committed nodes keep their DOM identity and their highlight), and the
 * tail renders as a stable slice.
 *
 * `cache` is mutated in place — entries beyond the committed count are
 * pruned (paragraphs folded back into the tail on fence open).
 * Pure — render fn injected for testability.
 */
export function renderIncremental(
  parts: string[],
  tail: string,
  cache: string[],
  render: (src: string) => string,
  fenceCache?: FenceCache,
): IncrementalResult {
  for (let i = cache.length; i < parts.length; i++) {
    cache.push(render(parts[i]));
  }
  // A fold-back pulled committed parts into the tail — prune to match.
  if (cache.length > parts.length) cache.length = parts.length;
  const tailHtml = tail ? renderStableSlice(tail, render, fenceCache) : '';
  return { committed: parts.length, tailHtml };
}

/**
 * Render a partial slice without unstable markdown structures.
 *
 * Marked re-parses the whole slice each frame, and its output restructures
 * when an unclosed ``` construct closes (literal backticks → <code> block)
 * — revealing INTO the construct makes the visible text re-render. When the
 * slice ends inside an open construct (odd number of ```), render the
 * prefix normally and the open construct as ESCAPED TEXT inside a code
 * block: the structure is stable frame-to-frame, the code types in
 * progressively (no freeze until the construct closes), and when the
 * closing ``` arrives the slice renders normally in one final pass.
 *
 * Pure — no DOM access; render fn injected for testability.
 */
export function renderStableSlice(
  visible: string,
  render: (src: string) => string,
  fenceCache?: FenceCache,
): string {
  const ticks = visible.match(/```/g) || [];
  if (ticks.length % 2 === 0) {
    return render(visible);
  }
  // Odd count = inside an open construct, starting at the last ```.
  const open = visible.lastIndexOf('```');
  const stablePrefix = visible.slice(0, open);
  // The prefix is constant while the block grows incrementally — cache its
  // HTML and escape only the growing tail each frame (avoids the O(n²) of
  // re-rendering the whole prefix per frame).
  let prefixHtml: string;
  if (fenceCache && fenceCache.key === stablePrefix) {
    prefixHtml = fenceCache.html;
  } else {
    prefixHtml = stablePrefix ? render(stablePrefix) : '';
    if (fenceCache) {
      fenceCache.key = stablePrefix;
      fenceCache.html = prefixHtml;
    }
  }
  // The open construct is escaped — it types in as literal text inside a
  // code block; DOMPurify-safe since every dynamic char is escaped.
  return prefixHtml + '<pre><code>' + escapeHtml(visible.slice(open)) + '</code></pre>';
}
