/**
 * markdown.ts — Markdown rendering and HTML escaping.
 *
 * Renders GFM markdown via marked, sanitizes with DOMPurify, and falls
 * back to plain escaped text on parse errors.
 *
 * Code blocks are enhanced after sanitization: completed `<pre><code>` blocks
 * get syntax highlighting (tree-shaken highlight.js) and a copy button. This
 * is safe for the streaming reveal because committed paragraphs render exactly
 * once (cached), and partial/open-fence code in the live tail is emitted by
 * `renderStableSlice` as escaped literal text (no `<pre>` to enhance).
 *
 * Provides: renderMarkdown, escapeHtml
 */
import { Marked } from 'marked';
import DOMPurify from 'dompurify';
import { highlightCode } from './highlight';

/** Module-level instance: options configure once, not per call. */
const md = new Marked({
  gfm: true,
  breaks: true,
});

export function renderMarkdown(text: string): string {
  try {
    const raw = md.parse(text) as string;
    const safe = DOMPurify.sanitize(raw, {
      USE_PROFILES: { html: true },
      // The html profile keeps interactive/UI elements that can be abused
      // for phishing (fake forms/inputs) or layout disruption (style) even
      // though event handlers are stripped — forbid them outright.
      FORBID_TAGS: ['form', 'input', 'button', 'select', 'textarea', 'style'],
    }) as string;
    return enhanceHtml(safe);
  } catch {
    return escapeHtml(text);
  }
}

/**
 * Post-process sanitized markdown HTML: wrap tables in an overflow container
 * (narrow screens scroll horizontally instead of overflowing) and enhance
 * completed code blocks (highlight + language badge + copy button).
 * Pure: string in → string out, so it respects the stable-prefix caching.
 */
export function enhanceHtml(html: string): string {
  // Wrap tables FIRST (block-level), then enhance code inside them too.
  return enhanceCode(wrapTables(html));
}

/** Wrap each top-level table in a scrollable container. Markdown cannot nest
 * tables, so a non-greedy match is safe. */
export function wrapTables(html: string): string {
  return html.replace(
    /<table>([\s\S]*?)<\/table>/g,
    '<div class="table-wrap"><table>$1</table></div>',
  );
}
export function enhanceCode(html: string): string {
  // Body is entity-escaped and may span lines → [\s\S]*?.
  const prePattern = /<pre><code( class="language-([^"]+)")?>([\s\S]*?)<\/code><\/pre>/g;
  return html.replace(prePattern, (_full, _cls, lang, body) => {
    const highlighted = lang ? highlightCode(lang, body) : body;
    return (
      '<pre' +
      // The badge floats at the top of the pre — has-code-lang makes the pre
      // reserve headroom, otherwise the badge covers the first code line.
      (lang ? ' class="has-code-lang"' : '') +
      '><code class="hljs' +
      (lang ? ` language-${lang}` : '') +
      '">' +
      highlighted +
      '</code>' +
      (lang ? `<span class="code-lang" aria-hidden="true">${lang}</span>` : '') +
      COPY_BUTTON +
      '</pre>'
    );
  });
}

/** Injected into each code block; reads the sibling code text on click. */
const COPY_BUTTON =
  '<span class="code-copy" role="button" tabindex="0" title="Copy code" aria-label="Copy code"></span>';

export function escapeHtml(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}
