/**
 * App.tsx — Root component: top bar, sidebar layer, chat body, global
 * cancel. Dialogs are first-party React components.
 *
 * Layout: a column — the always-visible TopBar on top, then the body row
 * (sidebar layer + chat). The sidebar layer serves two regimes: a normal
 * flex column on desktop (drag-resizable, hidden when collapsed) and an
 * overlay drawer on narrow viewports (<768px — the app's ONE breakpoint).
 * Ctrl/Cmd+B toggles; the choice persists via core/prefs.
 *
 * Provides: App
 * Depends: components/TopBar.tsx, components/Sidebar.tsx,
 *          components/ChatView.tsx, components/ErrorBoundary.tsx,
 *          components/RightDock.tsx, core/state.ts, core/prefs.ts,
 *          hooks/useEscapeKey.ts
 */
import { useCallback, useEffect, useLayoutEffect } from 'react';
import { TopBar } from './TopBar';
import { Sidebar } from './Sidebar';
import { ChatView } from './ChatView';
import { ErrorBoundary } from './ErrorBoundary';
import { RightDock } from './RightDock';
import { Toasts } from './Toasts';
import { TooltipProvider } from './ui/tooltip';
import { useFlux } from '../core/state';
import { restoreTerminals } from '../services/terminal';
import { useEscapeKey } from '../hooks/useEscapeKey';
import { useEdgeResize, edgeResizeKeys, type EdgeResizeSpec } from '../hooks/useEdgeResize';
import { isMobileViewport } from '../hooks/useIsMobile';
import { bridge } from '../core/bridge';
import { discardInterrupt } from '../services/stream-handler';
import { storeSidebarWidth, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH } from '../core/prefs';
import { cn } from '../lib/cn';

export function App(): React.ReactElement {
  const sidebarOpen = useFlux((s) => s.sidebarOpen);
  const sidebarWidth = useFlux((s) => s.sidebarWidth);
  const previewWidth = useFlux((s) => s.previewWidth);
  const connectionStatus = useFlux((s) => s.connectionStatus);
  const activeChatId = useFlux((s) => s.activeChatId);

  // Escape key cancels streaming — and retires any interrupt-send
  // bookkeeping: after an explicit stop the queued turn dies server-side
  // ("stop means stop"), so the wrap-up must show the usual notice.
  // Guards: when a Radix surface (dialog / dropdown / select) is open, ITS
  // layer owns Escape — DismissableLayer handles the keydown on its content
  // element (and prevents default when it dismisses), so an event that
  // arrives here already-consumed, or with such a surface still mounted,
  // must not ALSO cancel the live round (closing the Settings dialog used
  // to kill a streaming answer).
  const onEscape = useCallback((e: KeyboardEvent) => {
    if (e.defaultPrevented) return;
    if (
      document.querySelector(
        '[role="dialog"][data-state="open"], [role="menu"], [role="listbox"]',
      )
    )
      return;
    const { activeChatId, streaming, sidebarOpen } = useFlux.getState();
    // The mobile drawer is the topmost surface while open and has no Radix
    // layer of its own — Escape closes it first, and must not ALSO cancel
    // the live round underneath. Desktop Escape falls through to cancel.
    if (sidebarOpen && isMobileViewport()) {
      useFlux.setState({ sidebarOpen: false });
      return;
    }
    if (activeChatId && streaming[activeChatId]) {
      discardInterrupt(activeChatId);
      bridge.send({ type: 'cancel', chat_id: activeChatId });
    }
  }, []);
  useEscapeKey(onEscape);

  // Ctrl/Cmd+B toggles the sidebar; Ctrl/Cmd+J toggles the file &
  // terminal dock (VS Code's panel chord — the two edges pair).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey)) return;
      const key = e.key.toLowerCase();
      if (key === 'b') {
        e.preventDefault();
        useFlux.setState({ sidebarOpen: !useFlux.getState().sidebarOpen });
      } else if (key === 'j') {
        e.preventDefault();
        useFlux.setState({ dockOpen: !useFlux.getState().dockOpen });
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  // Publish the widths as CSS variables (#sidebar and #right-dock consume
  // them; the narrow drawer/sheet modes override with their own widths).
  // useLayoutEffect: the vars must be set BEFORE first paint or persisted
  // non-default widths flash the fallbacks.
  useLayoutEffect(() => {
    document.documentElement.style.setProperty('--fx-sidebar-w', `${sidebarWidth}px`);
  }, [sidebarWidth]);
  useLayoutEffect(() => {
    document.documentElement.style.setProperty('--fx-preview-w', `${previewWidth}px`);
  }, [previewWidth]);

  // The drag contract (CSS-var direct write, release commit, body class)
  // lives in the shared hook — the sidebar tracks the pointer's x. The
  // keyboard contract rides the same spec: the separator is focusable and
  // the arrows nudge ±24px (the dock's separator behaves identically).
  const sidebarSpec: EdgeResizeSpec = {
    cssVar: '--fx-sidebar-w',
    dragClass: 'resizing-sidebar',
    widthAt: (e) => e.clientX,
    clamp: (x: number) => Math.min(SIDEBAR_MAX_WIDTH, Math.max(SIDEBAR_MIN_WIDTH, x)),
    commit: (next: number) => {
      useFlux.setState({ sidebarWidth: next });
      storeSidebarWidth(next);
    },
  };
  const startResize = useEdgeResize(sidebarSpec);
  const resizeKeys = edgeResizeKeys(
    sidebarSpec,
    () => useFlux.getState().sidebarWidth,
    'right', // the handle is the sidebar's RIGHT border — ArrowRight widens
  );

  // Terminal tabs survive a refresh via sessionStorage + the server's
  // grace reattach — restore (and reconnect) them once the identity is
  // live. Guarded per chat inside the service.
  useEffect(() => {
    if (connectionStatus === 'connected' && activeChatId) {
      void restoreTerminals(activeChatId);
    }
  }, [connectionStatus, activeChatId]);

  return (
    <TooltipProvider delayDuration={300}>
      <ErrorBoundary>
        <div
          className={cn(
            // `min-w-0` is LOAD-BEARING: without it this flex row item's
            // automatic minimum (min-content) lets pathological message
            // content — an unbreakable token, a wide table — stretch the
            // whole shell wider than the viewport (mobile scrollbar).
            'flex min-h-0 min-w-0 flex-1 flex-col',
            sidebarOpen ? 'sidebar-open' : 'sidebar-closed',
          )}
        >
          <TopBar />
          <div className="relative flex min-h-0 flex-1">
            <div id="sidebar-layer" className={sidebarOpen ? 'open' : 'closed'}>
              {/* Narrow-viewport backdrop: click-through closes the drawer.
                  Desktop CSS keeps it display:none. */}
              <div id="sidebar-backdrop" onClick={() => useFlux.setState({ sidebarOpen: false })} />
              <Sidebar />
              {/* Drag handle: desktop only (narrow CSS hides it). Focusable
                  with aria-valuenow — a focusable separator is a splitter
                  and must expose its value; the arrows resize (B3). */}
              <div
                id="sidebar-resizer"
                role="separator"
                aria-orientation="vertical"
                aria-label="Resize sidebar"
                aria-valuenow={sidebarWidth}
                aria-valuemin={SIDEBAR_MIN_WIDTH}
                aria-valuemax={SIDEBAR_MAX_WIDTH}
                tabIndex={0}
                onPointerDown={startResize}
                onKeyDown={resizeKeys}
              />
            </div>
            <ErrorBoundary>
              <ChatView />
            </ErrorBoundary>
            {/* The right dock: multi-file preview tabs + the chat terminal.
                A docked flex sibling of the chat column — widening it pushes
                the conversation left instead of covering it. Narrow viewports
                flip it to a full-height overlay via the #right-dock media
                query in app.css (a push layout would crush the conversation
                there). */}
            <RightDock />
          </div>
          {/* Unified non-blocking notifications (filesystem surfaces) — fixed
              stack, top right under the bar; fixed is out of flow so its
              position in the tree carries no layout. */}
          <Toasts />
        </div>
      </ErrorBoundary>
    </TooltipProvider>
  );
}
