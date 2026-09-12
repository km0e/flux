import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { installCodeCopyHandler } from '../../services/code-copy';

describe('installCodeCopyHandler', () => {
  let container: HTMLDivElement;
  let writeText: ReturnType<typeof vi.fn>;
  let teardown: () => void;

  beforeEach(() => {
    container = document.createElement('div');
    document.body.appendChild(container);
    writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText },
      configurable: true,
    });
    teardown = installCodeCopyHandler(container);
  });

  afterEach(() => {
    teardown();
    container.remove();
  });

  function addBlock(code: string): HTMLElement {
    container.innerHTML = `<pre><code>${code}</code><span class="code-copy" aria-label="Copy code"></span></pre>`;
    return container.querySelector('.code-copy') as HTMLElement;
  }

  it('copies the sibling code text on click', () => {
    const btn = addBlock('const x = 1');
    btn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(writeText).toHaveBeenCalledWith('const x = 1');
  });

  it('ignores clicks not on a code-copy element', () => {
    container.innerHTML = '<div>no button</div>';
    container.querySelector('div')!.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(writeText).not.toHaveBeenCalled();
  });

  it('copies via Enter key on the button', () => {
    const btn = addBlock('echo hi');
    btn.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    expect(writeText).toHaveBeenCalledWith('echo hi');
  });

  it('flips to copied state on success and back after 1.5s', async () => {
    vi.useFakeTimers();
    try {
      const btn = addBlock('x');
      btn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      // Copied class is set inside the clipboard .then() — flush microtasks.
      await vi.advanceTimersByTimeAsync(0);
      expect(btn.classList.contains('copied')).toBe(true);
      await vi.advanceTimersByTimeAsync(1500);
      expect(btn.classList.contains('copied')).toBe(false);
    } finally {
      vi.useRealTimers();
    }
  });

  it('stops handling after teardown', () => {
    teardown();
    const btn = addBlock('y');
    btn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(writeText).not.toHaveBeenCalled();
  });
});
