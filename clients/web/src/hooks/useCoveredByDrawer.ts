/**
 * useCoveredByDrawer — whether the mobile sidebar drawer (the app's ONE
 * breakpoint, <768px overlay regime) currently covers this surface.
 *
 * The drawer is a fixed overlay there, but NOT a Radix layer: without
 * help, keyboard/AT focus walks straight through it into the main content
 * it visually covers. Covered surfaces go `inert` (out of the tab order,
 * the a11y tree AND pointer events — React 19's boolean attribute), which
 * keeps the drawer's own controls as the only reachable thing. The TopBar
 * deliberately stays outside: its toggle is how the drawer closes.
 *
 * Desktop is never covered — there the sidebar is a side-by-side flex
 * sibling, not an overlay.
 *
 * Provides: useCoveredByDrawer
 * Depends: hooks/useIsMobile.ts, core/state.ts
 */
import { useIsMobile } from './useIsMobile';
import { useFlux } from '../core/state';

export function useCoveredByDrawer(): boolean {
  const isMobile = useIsMobile();
  const sidebarOpen = useFlux((s) => s.sidebarOpen);
  return isMobile && sidebarOpen;
}
