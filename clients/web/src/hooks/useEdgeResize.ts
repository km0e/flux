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
    const move = (ev: PointerEvent) => {
      document.documentElement.style.setProperty(
        spec.cssVar,
        `${spec.clamp(spec.widthAt(ev))}px`,
      );
    };
    const up = (ev: PointerEvent) => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
      document.body.classList.remove(spec.dragClass);
      spec.commit(spec.clamp(spec.widthAt(ev)));
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };
}
