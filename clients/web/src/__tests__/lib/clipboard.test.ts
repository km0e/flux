/**
 * clipboard.test.ts — copyText's secure-context fallback.
 *
 * navigator.clipboard exists only in secure contexts; copyText must fall
 * back to execCommand('copy') when it is missing (plain-HTTP LAN
 * deployments) instead of failing silently.
 *
 * Provides: copyText behavior tests
 * Depends: ../lib/clipboard
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { copyText } from '../../lib/clipboard';

function setClipboard(value: unknown): void {
  Object.defineProperty(navigator, 'clipboard', { value, configurable: true });
}

describe('copyText', () => {
  beforeEach(() => {
    document.body.innerHTML = '';
    vi.restoreAllMocks();
  });

  it('prefers navigator.clipboard when available', async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    setClipboard({ writeText });
    await expect(copyText('hello')).resolves.toBe(true);
    expect(writeText).toHaveBeenCalledWith('hello');
  });

  it('falls back to execCommand when the Clipboard API is missing', async () => {
    setClipboard(undefined);
    const exec = vi.fn().mockReturnValue(true);
    document.execCommand = exec as unknown as typeof document.execCommand;
    await expect(copyText('hello')).resolves.toBe(true);
    expect(exec).toHaveBeenCalledWith('copy');
    // The temporary textarea is cleaned up.
    expect(document.querySelectorAll('textarea')).toHaveLength(0);
  });

  it('reports failure when every path fails', async () => {
    setClipboard(undefined);
    document.execCommand = vi.fn().mockReturnValue(false) as unknown as typeof document.execCommand;
    await expect(copyText('hello')).resolves.toBe(false);
  });
});
