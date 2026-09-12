/**
 * setup.ts — vitest global setup: matchers, auto-cleanup, stable viewport.
 */
import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterEach } from 'vitest';

// RTL's automatic cleanup registers via global afterEach — which requires
// vitest globals. We run with globals: false, so register it here.
afterEach(() => {
  cleanup();
});

// jsdom has no layout engine; code paths that read innerWidth (sidebar
// drawer mode) get a desktop-sized default.
Object.defineProperty(window, 'innerWidth', { value: 1280, writable: true, configurable: true });

// ── jsdom gaps that Radix primitives (and the virtualized tree) need ──

class MockResizeObserver {
  callback: ResizeObserverCallback;
  constructor(cb: ResizeObserverCallback) {
    this.callback = cb;
  }
  observe(target: Element) {
    // Fire once with the element's (zero) size — enough for mounts.
    this.callback(
      [{ target, contentRect: { width: 300, height: 300 } } as ResizeObserverEntry],
      this as unknown as ResizeObserver,
    );
  }
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = MockResizeObserver as unknown as typeof ResizeObserver;

// PointerEvent: jsdom has no implementation; Radix's dismissable layers
// and dropdown trigger read pointer coordinates/types.
class MockPointerEvent extends MouseEvent {
  pointerId: number;
  pointerType: string;
  isPrimary: boolean;
  constructor(type: string, params: PointerEventInit = {}) {
    super(type, params);
    this.pointerId = params.pointerId ?? 1;
    this.pointerType = params.pointerType ?? 'mouse';
    this.isPrimary = params.isPrimary ?? true;
  }
}
globalThis.PointerEvent = MockPointerEvent as unknown as typeof PointerEvent;

window.HTMLElement.prototype.hasPointerCapture = () => false;
window.HTMLElement.prototype.setPointerCapture = () => {};
window.HTMLElement.prototype.releasePointerCapture = () => {};
window.HTMLElement.prototype.scrollIntoView = () => {};
