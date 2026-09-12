/**
 * useEscapeKey.ts — shared Escape handler (IME-composition safe).
 */
import { useEffect } from 'react';

export function useEscapeKey(handler: (e: KeyboardEvent) => void): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // IME candidate dismissal must not trigger app-level escapes.
      if (e.isComposing || (e as KeyboardEvent & { keyCode: number }).keyCode === 229) return;
      handler(e);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [handler]);
}
