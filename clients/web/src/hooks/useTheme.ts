/**
 * useTheme.ts — the theme-cycling hook (a hooks-layer concern: state +
 * side effects, no markup).
 *
 * Cycles auto → dark → light; explicit values pin `data-theme` on <html>,
 * auto clears it (the media query owns it — the inline bootstrap in
 * index.html handled the initial state). Live terminals re-read the
 * --fx-* tokens into their palette AFTER the attribute flip (the tokens
 * are theme-aware; the canvas atlas only rebuilds on a re-read).
 *
 * Provides: useTheme
 * Depends: core/prefs.ts, services/terminal.ts
 */
import { useState } from 'react';
import { readStoredTheme, storeTheme, type ThemeChoice } from '../core/prefs';
import { applyTerminalTheme } from '../services/terminal';

const NEXT_THEME: Record<ThemeChoice, ThemeChoice> = {
  auto: 'dark',
  dark: 'light',
  light: 'auto',
};

export function useTheme(): [ThemeChoice, () => void] {
  const [theme, setTheme] = useState<ThemeChoice>(() => readStoredTheme() ?? 'auto');
  const cycle = () => {
    const next = NEXT_THEME[theme];
    setTheme(next);
    storeTheme(next);
    // The inline bootstrap in index.html handled the initial state; explicit
    // choices pin the attribute, auto clears it (the media query owns it).
    if (next === 'auto') delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = next;
    // AFTER the attribute flip, so getComputedStyle sees the new palette.
    applyTerminalTheme();
  };
  return [theme, cycle];
}
