/**
 * useEdgeResize — pointer drag-resize for the width-authored edges
 * (the sidebar, the right dock).
 *
 * The two surfaces shared an identical drag contract that used to live as
 * a copy in each component:
 *
 *   - the width rides the CSS var DIRECTLY during the drag — zero React
 *     renders per pointermove (the jank this pattern exists to kill);
 *   - the store commit + persistence happen ONCE, on release;
 *   - a body class during the drag kills text selection.
 *
 * The hook returns the `onPointerDown` handler parametrized by the spec;
 * the drag contract itself lives here, once.
 *
 * Provides: useEdgeResize
 * Depends: (react only)
 */

/** The drag specification — everything that differs between the edges. */
export interface EdgeResizeSpec {
  /** The CSS variable the drag writes directly (px values). */
  cssVar: string;
  /** The body class while dragging (kills text selection). */
  dragClass: string;
  /** The raw width for a pointer position — the sidebar tracks the
   * pointer's x; the dock tracks its distance from the right edge. */
  widthAt: (e: PointerEvent) => number;
  /** Clamp a raw width into the legal band. */
  clamp: (width: number) => number;
  /** Commit on release — the store write + persistence. */
  commit: (width: number) => void;
}

export function useEdgeResize(spec: EdgeResizeSpec): (e: React.PointerEvent) => void {
  return (e: React.PointerEvent) => {
    e.preventDefault();
    document.body.classList.add(spec.dragClass);
    // The last width the pointer MOVED to. A pointercancel event carries no
    // meaningful coordinates (browsers zero them), so the cancel path
    // commits THIS — never widthAt(cancelEvent).
    let lastWidth: number | null = null;
    const detach = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      window.removeEventListener('pointercancel', cancel);
      document.body.classList.remove(spec.dragClass);
    };
    const move = (ev: PointerEvent) => {
      lastWidth = spec.clamp(spec.widthAt(ev));
      document.documentElement.style.setProperty(spec.cssVar, `${lastWidth}px`);
    };
    const up = (ev: PointerEvent) => {
      detach();
      spec.commit(spec.clamp(spec.widthAt(ev)));
    };
    // A cancelled gesture (touch interrupted by an incoming call, the
    // browser claiming the pointer): no release happened, but the CSS var
    // already shows the dragged width — commit the last moved value so the
    // visible width and the store never disagree (or nothing, if the
    // pointer never moved). The listeners MUST come off either way, or the
    // next pointermove drags a dead gesture.
    const cancel = () => {
      detach();
      if (lastWidth !== null) spec.commit(lastWidth);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
    window.addEventListener('pointercancel', cancel);
  };
}

/** Keyboard resize for a FOCUSABLE separator: ±24px per press, committed at
 * once (no drag session). `growOn` names the arrow key that GROWS the
 * surface — the direction the drag handle moves: the sidebar's handle is
 * its right border (ArrowRight grows), the dock's is its left border
 * (ArrowLeft grows). Other keys pass through untouched. */
export function edgeResizeKeys(
  spec: EdgeResizeSpec,
  widthNow: () => number,
  growOn: 'left' | 'right',
): (e: React.KeyboardEvent) => void {
  return (e) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault();
    const grow = e.key === (growOn === 'left' ? 'ArrowLeft' : 'ArrowRight');
    const next = spec.clamp(widthNow() + (grow ? 24 : -24));
    if (next === widthNow()) return; // already at the clamp — no-op press
    document.documentElement.style.setProperty(spec.cssVar, `${next}px`);
    spec.commit(next);
  };
}
