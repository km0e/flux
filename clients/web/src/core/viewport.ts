/**
 * viewport.ts — keyboard-safe viewport height for mobile.
 *
 * `100dvh` tracks the URL bar but NOT the keyboard: on iOS the keyboard
 * overlays the layout viewport, so a bottom-docked composer stays buried
 * no matter what vh unit is used. The mature fix is to publish the
 * visualViewport height as a CSS variable and let the app root consume it
 * (`height: var(--fx-vvh, 100dvh)` in app.css). Chrome Android is covered
 * by `interactive-widget=resizes-content` in the viewport meta — there the
 * visual viewport equals the (shrunk) layout viewport, so the same
 * variable is consistent on both platforms.
 *
 * Framework-free: called once from mount.ts, plain listeners, no React.
 * Browsers without visualViewport keep the pure-CSS fallback chain.
 *
 * Provides: initViewportHeight
 */

/** Slack below which a resize is treated as keyboard noise (scrollbars,
 *  rounded-px jitter) — avoids style writes per pixel during gestures. */
const EPSILON_PX = 1;

export function initViewportHeight(): void {
  const vv = window.visualViewport;
  if (!vv) return; // CSS fallback chain owns it
  let last = -1;
  const apply = () => {
    const h = Math.round(vv.height);
    if (Math.abs(h - last) < EPSILON_PX) return;
    last = h;
    document.documentElement.style.setProperty('--fx-vvh', `${h}px`);
  };
  vv.addEventListener('resize', apply);
  // iOS pans the visual viewport when the keyboard opens (offsetTop moves);
  // the height is what matters — re-assert after pan settles too.
  vv.addEventListener('scroll', apply);
  apply();
}
