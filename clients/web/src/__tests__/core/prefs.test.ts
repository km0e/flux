/**
 * prefs.test.ts — localStorage-backed UI preferences.
 *
 * Provides: prefs round-trip + clamping tests
 * Depends: core/prefs
 */
import { describe, it, expect, beforeEach, afterEach } from 'vitest';
import {
  readStoredSidebarOpen,
  storeSidebarOpen,
  readStoredSidebarWidth,
  storeSidebarWidth,
  readStoredPreviewWidth,
  storePreviewWidth,
  SIDEBAR_MIN_WIDTH,
  SIDEBAR_MAX_WIDTH,
  PREVIEW_MIN_WIDTH,
  PREVIEW_MAX_WIDTH,
  CONVERSATION_MIN_WIDTH,
  clampSidebarWidth,
  clampPreviewWidth,
} from '../../core/prefs';

describe('prefs', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    localStorage.clear();
  });

  it('sidebar open round-trips', () => {
    expect(readStoredSidebarOpen()).toBeNull();
    storeSidebarOpen(false);
    expect(readStoredSidebarOpen()).toBe(false);
    storeSidebarOpen(true);
    expect(readStoredSidebarOpen()).toBe(true);
  });

  it('sidebar width round-trips and clamps to the legal band', () => {
    expect(readStoredSidebarWidth()).toBeNull();
    storeSidebarWidth(280);
    expect(readStoredSidebarWidth()).toBe(280);
    storeSidebarWidth(SIDEBAR_MIN_WIDTH - 100);
    expect(readStoredSidebarWidth()).toBe(SIDEBAR_MIN_WIDTH);
    storeSidebarWidth(SIDEBAR_MAX_WIDTH + 500);
    expect(readStoredSidebarWidth()).toBe(SIDEBAR_MAX_WIDTH);
  });

  it('a corrupt width reads as null, not a crash', () => {
    localStorage.setItem('flux.sidebar.width', 'not-a-number');
    expect(readStoredSidebarWidth()).toBeNull();
  });

  it('preview width round-trips and clamps to the legal band', () => {
    expect(readStoredPreviewWidth()).toBeNull();
    storePreviewWidth(640);
    expect(readStoredPreviewWidth()).toBe(640);
    storePreviewWidth(PREVIEW_MIN_WIDTH - 100);
    expect(readStoredPreviewWidth()).toBe(PREVIEW_MIN_WIDTH);
    storePreviewWidth(PREVIEW_MAX_WIDTH + 500);
    expect(readStoredPreviewWidth()).toBe(PREVIEW_MAX_WIDTH);
  });

  describe('the pane-aware clamps (tablet walk-through)', () => {
    it('the dock leaves the conversation its floor BEHIND BOTH panes', () => {
      // 800px viewport, 240px sidebar: the dock may take at most 280.
      expect(clampPreviewWidth(520, 800, 240)).toBe(280);
      // A drag past the floor lands exactly on it; below the pane's own
      // minimum the floor catches it.
      expect(clampPreviewWidth(300, 800, 240)).toBe(280);
      expect(clampPreviewWidth(260, 800, 240)).toBe(280);
      // Wide viewport: the familiar band applies untouched.
      expect(clampPreviewWidth(900, 1600, 240)).toBe(900);
      expect(clampPreviewWidth(2000, 1600, 240)).toBe(PREVIEW_MAX_WIDTH);
    });

    it('an open dock bounds the sidebar symmetrically', () => {
      // 800px viewport with a 480px dock: the sidebar can only shrink —
      // its own minimum wins over a viewport that has no room.
      expect(clampSidebarWidth(360, 800, 480)).toBe(SIDEBAR_MIN_WIDTH);
      expect(clampSidebarWidth(200, 800, 480)).toBe(SIDEBAR_MIN_WIDTH);
      // Dock closed (0): the full band applies even at 800px.
      expect(clampSidebarWidth(360, 800, 0)).toBe(SIDEBAR_MAX_WIDTH);
      expect(clampSidebarWidth(100, 800, 0)).toBe(SIDEBAR_MIN_WIDTH);
    });

    it('at the desktop breakpoint edge the three-way floor still holds', () => {
      // 768px, sidebar 240: the drag clamp holds the dock at its own
      // minimum (the render-time min() in app.css narrows it further to
      // 248 so the CONVERSATION keeps the full floor).
      const dock = clampPreviewWidth(480, 768, 240);
      expect(dock).toBe(PREVIEW_MIN_WIDTH);
      // Sidebar drag with the dock at its floor: the sidebar yields.
      expect(clampSidebarWidth(360, 768, dock)).toBe(208);
      expect(768 - 208 - dock).toBe(CONVERSATION_MIN_WIDTH);
    });
  });
});
