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
});
