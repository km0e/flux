/**
 * theme.ts — theme APPLICATION, split from the choice.
 *
 * The choice is global store state (`theme` in core/state — promoted out
 * of the old useTheme local useState so the command palette's cycle and
 * the TopBar button drive ONE truth); this service owns what the choice
 * DOES: pin `data-theme` on <html> (explicit values) or clear it (auto —
 * the media query owns it; index.html's inline bootstrap handled the
 * first paint), then re-read the live terminals' palette AFTER the flip.
 *
 * Provides: applyTheme, cycleTheme, THEME_LABEL
 * Depends: core/state.ts, services/terminal.ts, core/prefs.ts (type)
 */
import { useFlux } from '../core/state';
import { applyTerminalTheme } from './terminal';
import type { ThemeChoice } from '../core/prefs';

export const THEME_LABEL: Record<ThemeChoice, string> = {
  auto: 'Auto',
  dark: 'Dark',
  light: 'Light',
};

const NEXT_THEME: Record<ThemeChoice, ThemeChoice> = {
  auto: 'dark',
  dark: 'light',
  light: 'auto',
};

/** Apply the store's current choice to the document + the terminals. */
export function applyTheme(): void {
  const t = useFlux.getState().theme;
  if (t === 'auto') delete document.documentElement.dataset.theme;
  else document.documentElement.dataset.theme = t;
  // AFTER the attribute flip, so getComputedStyle sees the new palette.
  applyTerminalTheme();
}

/** auto → dark → light → auto, persisted through the store action. */
export function cycleTheme(): void {
  useFlux.getState().setTheme(NEXT_THEME[useFlux.getState().theme]);
  applyTheme();
}
