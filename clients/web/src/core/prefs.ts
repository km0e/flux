/**
 * prefs.ts — localStorage-backed UI preferences.
 *
 * Deliberately localStorage (NOT sessionStorage like session.ts): sidebar
 * collapse/width are device-level preferences that survive browser restarts,
 * while the session identity must stay per-tab. All accessors swallow storage
 * failures (private mode) so the UI degrades to defaults.
 *
 * Provides: readStoredSidebarOpen, storeSidebarOpen, readStoredSidebarWidth,
 *           storeSidebarWidth
 */
const SIDEBAR_OPEN_KEY = 'flux.sidebar.open';
const THEME_KEY = 'flux.theme';
const SIDEBAR_WIDTH_KEY = 'flux.sidebar.width';
const PREVIEW_WIDTH_KEY = 'flux.preview.width';

/** Sidebar width clamp — shared by the resizer and the stored-value reader. */
export const SIDEBAR_MIN_WIDTH = 160;
export const SIDEBAR_MAX_WIDTH = 360;
export const SIDEBAR_DEFAULT_WIDTH = 240;

/** Preview dock width clamp + default — same shared-band pattern. */
export const PREVIEW_MIN_WIDTH = 280;
export const PREVIEW_MAX_WIDTH = 1080;
export const PREVIEW_DEFAULT_WIDTH = 480;

/** The persisted collapse choice, if the user ever toggled explicitly. */
export function readStoredSidebarOpen(): boolean | null {
  try {
    const v = localStorage.getItem(SIDEBAR_OPEN_KEY);
    return v === null ? null : v === '1';
  } catch {
    return null;
  }
}

export function storeSidebarOpen(open: boolean): void {
  try {
    localStorage.setItem(SIDEBAR_OPEN_KEY, open ? '1' : '0');
  } catch {
    // storage unavailable — the preference simply won't persist
  }
}

/** The persisted width, clamped to the legal band; null when unset/corrupt. */
export function readStoredSidebarWidth(): number | null {
  try {
    const raw = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    if (raw === null) return null;
    const n = Number.parseInt(raw, 10);
    if (!Number.isFinite(n)) return null;
    return Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, n));
  } catch {
    return null;
  }
}

export function storeSidebarWidth(width: number): void {
  try {
    localStorage.setItem(SIDEBAR_WIDTH_KEY, String(width));
  } catch {
    // storage unavailable — the preference simply won't persist
  }
}

/** The persisted preview-dock width, clamped to the legal band. */
export function readStoredPreviewWidth(): number | null {
  try {
    const raw = localStorage.getItem(PREVIEW_WIDTH_KEY);
    if (raw === null) return null;
    const n = Number.parseInt(raw, 10);
    if (!Number.isFinite(n)) return null;
    return Math.min(PREVIEW_MAX_WIDTH, Math.max(PREVIEW_MIN_WIDTH, n));
  } catch {
    return null;
  }
}

export function storePreviewWidth(width: number): void {
  try {
    localStorage.setItem(PREVIEW_WIDTH_KEY, String(width));
  } catch {
    // storage unavailable — the preference simply won't persist
  }
}

// ── Theme (auto follows prefers-color-scheme; explicit wins) ──

export type ThemeChoice = 'auto' | 'light' | 'dark';

/** The persisted theme choice; null = never chosen (= auto). */
export function readStoredTheme(): ThemeChoice | null {
  try {
    const v = localStorage.getItem(THEME_KEY);
    return v === 'light' || v === 'dark' ? v : null;
  } catch {
    return null;
  }
}

export function storeTheme(t: ThemeChoice): void {
  try {
    // 'auto' removes the override entirely — the media query owns it again.
    if (t === 'auto') localStorage.removeItem(THEME_KEY);
    else localStorage.setItem(THEME_KEY, t);
  } catch {
    // storage unavailable — the preference simply won't persist
  }
}
