/**
 * highlight.ts — lazily-loaded, tree-shaken syntax highlighting.
 *
 * highlight.js (core + a pragmatic subset of the languages a coding agent
 * reply most commonly uses) lives in its own async chunk (`hljs-bundle.ts`)
 * and is PREFETCHED at app bootstrap: the dynamic import starts when the
 * bundle executes — long before the first code block can arrive (first
 * token latency covers the fetch), so the deferred weight never lands on
 * the entry chunk the first paint waits on.
 *
 * The render pipeline calls `highlightCode` SYNCHRONOUSLY on every frame,
 * so the loader keeps a sync surface: before the chunk lands (or for an
 * unregistered language) it degrades to the escaped code — a plain code
 * block, never unsafe, never a thrown error. Committed blocks render once
 * (append-only pipeline), so a block committed inside the tiny pre-ready
 * window stays plain; `renderHistoryMessages` awaits {@link preloadHighlighter}
 * explicitly, which is the only surface where that race is realistic.
 */
let hljs: (typeof import('./hljs-bundle'))['hljs'] | null = null;
let ready: Promise<void> | null = null;

/** Resolve once the highlighter chunk has loaded and registered. Idempotent. */
export function preloadHighlighter(): Promise<void> {
  ready ??= import('./hljs-bundle').then((m) => {
    hljs = m.hljs;
  });
  return ready;
}

// Boot-time prefetch — starts alongside the WS connect, not after it.
void preloadHighlighter();

/**
 * Highlight raw (already HTML-safe-escaped) code for a fence language suffix.
 * Returns highlighted HTML or, when the highlighter is not loaded yet / the
 * language is unknown / highlighting fails, the input `escapedCode` unchanged.
 * Never throws.
 */
export function highlightCode(lang: string, escapedCode: string): string {
  if (!hljs || !hljs.getLanguage(lang)) return escapedCode;
  try {
    // The lang was already validated through getLanguage (including alias
    // resolution) — pass it straight through; v11's highlight() resolves by
    // name/alias the same way.
    const { value } = hljs.highlight(unescapeEntities(escapedCode), {
      language: lang,
      ignoreIllegals: true,
    });
    return value;
  } catch {
    return escapedCode;
  }
}

/** Test hook: drop the loaded highlighter so the fallback path is testable. */
export function _resetHighlighterForTest(): void {
  hljs = null;
  ready = null;
}

/** Decode the HTML entities markdown emits inside `<code>` back to raw source. */
function unescapeEntities(text: string): string {
  return text
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, '&');
}
