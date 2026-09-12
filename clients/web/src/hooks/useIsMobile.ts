/**
 * useIsMobile — the app's ONE breakpoint (<768px, the `max-md:` regime)
 * as reactive state. Use for BEHAVIOR that CSS alone can't express (e.g.
 * different placeholder TEXT); layout stays in CSS via `max-md:` utilities.
 *
 * jsdom-safe: without matchMedia it reports false (desktop), so tests and
 * non-browser embeds get the desktop variant.
 */
import { useEffect, useState } from 'react';

const QUERY = '(max-width: 767px)';

export function useIsMobile(): boolean {
  const [isMobile, setIsMobile] = useState(
    () => typeof matchMedia === 'function' && matchMedia(QUERY).matches,
  );
  useEffect(() => {
    if (typeof matchMedia !== 'function') return;
    const mq = matchMedia(QUERY);
    const onChange = (e: MediaQueryListEvent) => setIsMobile(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return isMobile;
}
