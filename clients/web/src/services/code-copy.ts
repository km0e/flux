/**
 * code-copy.ts — Delegated "Copy code" handler for code-block copy buttons.
 *
 * The copy button is injected into rendered code blocks by `enhanceCode`
 * (markdown.ts) as a plain `<span class="code-copy">`. Because message HTML is
 * written via `innerHTML`, inline handlers would be stripped (and are CSP-
 * hostile), so we install ONE delegated listener on the messages container
 * here. Clicking `.code-copy` copies the source of the sibling `<code>` element
 * (textContent already yields the unescaped code) and flips the label briefly.
 */
import { log } from '../logger';
import { copyText } from '../lib/clipboard';

/** Install the delegation once; returns a teardown for tests. */
export function installCodeCopyHandler(container: HTMLElement): () => void {
  const onClick = (e: MouseEvent): void => {
    const el = (e.target as HTMLElement).closest<HTMLElement>('.code-copy');
    if (!el) return;
    e.preventDefault();
    const pre = el.closest('pre');
    const code = pre?.querySelector('code');
    const text = code?.textContent ?? '';
    if (!text) return;
    void copyText(text).then((ok) => {
      if (!ok) {
        log.warn('code copy failed');
        return;
      }
      el.classList.add('copied');
      el.setAttribute('aria-label', 'Copied');
      setTimeout(() => {
        el.classList.remove('copied');
        el.setAttribute('aria-label', 'Copy code');
      }, 1500);
    });
  };
  const onKey = (e: KeyboardEvent): void => {
    if (e.key !== 'Enter' && e.key !== ' ') return;
    const el = (e.target as HTMLElement).closest<HTMLElement>('.code-copy');
    if (!el) return;
    e.preventDefault();
    el.click();
  };
  container.addEventListener('click', onClick);
  container.addEventListener('keydown', onKey);
  return () => {
    container.removeEventListener('click', onClick);
    container.removeEventListener('keydown', onKey);
  };
}
