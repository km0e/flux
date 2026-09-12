import { describe, it, expect, beforeAll } from 'vitest';
import { renderMarkdown, escapeHtml } from '../../lib/markdown';
import { preloadHighlighter } from '../../lib/highlight';

// The highlighter is a deferred async chunk — tests asserting tokenized
// output wait for the boot-time prefetch to land (sync callers see the
// escaped fallback before that).
beforeAll(() => preloadHighlighter());

describe('escapeHtml', () => {
  it('escapes angle brackets', () => {
    expect(escapeHtml('<script>')).toBe('&lt;script&gt;');
  });

  it('escapes ampersands', () => {
    expect(escapeHtml('a & b')).toBe('a &amp; b');
  });

  it('escapes quotes', () => {
    expect(escapeHtml('"hello"')).toBe('&quot;hello&quot;');
  });

  it('returns empty string for empty input', () => {
    expect(escapeHtml('')).toBe('');
  });
});

describe('renderMarkdown', () => {
  it('renders plain text as paragraph', () => {
    const result = renderMarkdown('hello');
    expect(result).toContain('hello');
  });

  it('renders bold text', () => {
    const result = renderMarkdown('**bold**');
    expect(result).toContain('<strong>bold</strong>');
  });

  it('renders inline code', () => {
    const result = renderMarkdown('`code`');
    expect(result).toContain('<code>code</code>');
  });

  it('renders code blocks', () => {
    const result = renderMarkdown('```\nconst x = 1;\n```');
    expect(result).toContain('const x = 1');
  });

  it('highlights fenced code with a language and injects a copy button', () => {
    const result = renderMarkdown('```js\nconst x = 1;\n```');
    // With a language the block is wrapped with hljs and code is tokenized.
    expect(result).toContain('<pre class="has-code-lang"><code class="hljs language-js">');
    expect(result).toContain('<span class="hljs-keyword">const</span>');
    // A language badge is injected (A2), then the copy button last child.
    expect(result).toContain('<span class="code-lang" aria-hidden="true">js</span>');
    expect(result).toContain('</code><span class="code-lang"');
    expect(result).toContain('</span><span class="code-copy"');
    expect(result).toContain('</span></pre>');
  });

  it('keeps unlanaguaged fences un-highlighted but still gets a copy button', () => {
    const result = renderMarkdown('```\nplain text\n```');
    expect(result).toContain('<code class="hljs">plain text');
    expect(result).not.toContain('language-');
    expect(result).toContain('<span class="code-copy"');
  });

  it('falls back to escaped code when the language is not registered', () => {
    const result = renderMarkdown('```brainfuck\n>+<\n```');
    // brainfuck is not in our tree-shaken subset → body unchanged (no token spans),
    // preserved as entity-escaped text.
    expect(result).toContain('<code class="hljs language-brainfuck">&gt;+&lt;');
    expect(result).not.toContain('hljs-keyword');
  });

  it('sanitizes script tags', () => {
    const result = renderMarkdown('<script>alert("xss")</script>');
    expect(result).not.toContain('<script>');
  });

  it('forbids interactive/UI form elements', () => {
    const result = renderMarkdown(
      '<form action="/x"><input name="q"><button>Submit</button><textarea><select>t</select></form>',
    );
    expect(result).not.toContain('<form');
    expect(result).not.toContain('<input');
    expect(result).not.toContain('<button');
    expect(result).not.toContain('<textarea');
    expect(result).not.toContain('<select');
  });

  it('forbids style elements', () => {
    const result = renderMarkdown('<style>body{display:none}</style>');
    expect(result).not.toContain('<style');
  });

  it('wraps GFM tables in a scrollable container', () => {
    const result = renderMarkdown('| a | b |\n|---|---|\n| 1 | 2 |');
    expect(result).toContain('<div class="table-wrap"><table>');
  });

  it('handles empty input', () => {
    const result = renderMarkdown('');
    expect(result).toBe('');
  });
});
