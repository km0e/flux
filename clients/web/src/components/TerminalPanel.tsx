/**
 * TerminalPanel.tsx — one terminal session inside the right dock.
 *
 * The session is created by an explicit user ask (the dock's "+" or the
 * sidebar button — services/terminal.ts owns the lifecycle); this panel
 * only MOUNTS its persistent container and shows the status line. The
 * height chain is definite at every level (dock → panel → host), and the
 * status bar is a normal flow row so the terminal never renders under it.
 *
 * Provides: TerminalPanel
 * Depends: services/terminal.ts
 */
import { useEffect, useRef, useState } from 'react';
import { mountSession, terminalSession, type TermStatus } from '../services/terminal';
import { Skeleton } from './ui';

export function TerminalPanel(props: { tabId: string }): React.ReactElement {
  const hostRef = useRef<HTMLDivElement>(null);
  const [status, setStatus] = useState<TermStatus | undefined>(
    () => terminalSession(props.tabId)?.status,
  );

  // The service mutates session objects outside React — a light poll
  // repaints the status line, but ONLY on an actual transition
  // (connecting → running/exited/failed): statuses are stable between
  // transitions, so an idle terminal costs zero re-renders (the pulse
  // animations are CSS, they need no React). Skipping the state write
  // when the status is unchanged keeps the 300ms tick from re-rendering
  // forever.
  useEffect(() => {
    const timer = window.setInterval(() => {
      const next = terminalSession(props.tabId)?.status;
      setStatus((prev) => (prev === next ? prev : next));
    }, 300);
    return () => window.clearInterval(timer);
  }, [props.tabId]);

  // Mount the session's container and open xterm on an ATTACHED, sized
  // box (this fixes the truncated-height render: xterm needs real pixels).
  // Deps are tabId ONLY: the status poll re-renders must never re-run this
  // effect — its cleanup detaches the container, which kills the terminal
  // focus (the reported focus-steal). The await inside mountSession
  // handles the session's async readiness; `cancelled` guards the
  // post-unmount append.
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    let cancelled = false;
    void mountSession(props.tabId, host, () => cancelled);
    return () => {
      cancelled = true;
      terminalSession(props.tabId)?.container?.remove();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.tabId]);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div ref={hostRef} data-testid="terminal-host" className="relative min-h-0 flex-1 overflow-hidden">
        {/* First frame: the shell's opening lines are incoming — quiet
            shape placeholders, not a blank inset void. `undefined` is the
            not-yet-created session, 'connecting' the handshake; both mean
            an empty host. Gone the moment the session runs (xterm paints
            under them). */}
        {(status === undefined || status === 'connecting') && (
          <div className="absolute inset-0 flex flex-col gap-2.5 p-3">
            <Skeleton className="h-3 w-2/5" />
            <Skeleton className="h-3 w-3/5" />
            <Skeleton className="h-3 w-1/2" />
          </div>
        )}
      </div>
      <div className="flex items-center justify-between gap-2 border-t border-border bg-panel px-3 py-1.5">
        {status === 'exited' ? (
          <>
            <span className="text-xs text-muted">
              Shell exited (code {terminalSession(props.tabId)?.exitedCode ?? '?'})
            </span>
            <span className="text-2xs text-muted">Use “+” to open a new terminal</span>
          </>
        ) : status === 'failed' ? (
          <span className="text-xs text-danger">
            Terminal connection failed — try again in a moment
          </span>
        ) : status === 'running' ? (
          <span className="inline-flex items-center gap-1.5 text-2xs text-muted">
            <span aria-hidden="true" className="size-1.5 animate-pulse rounded-full bg-success" />
            running — backgrounded output keeps buffering
          </span>
        ) : (
          <span className="inline-flex items-center gap-1.5 text-2xs text-muted">
            <span aria-hidden="true" className="size-1.5 animate-pulse rounded-full bg-warn" />
            connecting…
          </span>
        )}
      </div>
    </div>
  );
}
