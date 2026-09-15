/**
 * mount.tsx — Mount the chat UI into the page.
 *
 * The single initialization: wires the ConnectConnection, the wire handler
 * registry, the lease-switching subscription, code-copy delegation, and the
 * React tree. Dialogs are first-party (components/dialogs — D-26: no host
 * abstraction) and register before connect.
 *
 * Ordering invariant (regression-guarded by mount.test): the App render MUST
 * complete (flushSync — React 18+ renders concurrently otherwise) BEFORE the
 * activeChatId restore runs. The subscription fires synchronously on restore
 * and its switchToChat path needs #messages-wrap to already exist; restoring
 * before the render used to throw 'messages-wrap not found' out of mountChat
 * — killing render AND connect, a blank page on every refresh with a
 * persisted active chat (the web UI's white-screen bug).
 *
 * Provides: mountChat, MountOptions
 * Depends: components/App.tsx, components/dialogs/impl.tsx, core/*, services/*
 */
import { createRoot } from 'react-dom/client';
import { flushSync } from 'react-dom';
import { App } from './components/App';
import { useFlux } from './core/state';
import { readStoredActiveChat, storeActiveChat } from './core/session';
import { storeSidebarOpen } from './core/prefs';
import { ConnectConnection } from './core/grpc-connection';
import { dispatchMessage } from './services/dispatch';
import { registerAllHandlers } from './services/handlers';
import { switchToChat } from './services/panes';
import { resetStreamingForReconnect } from './services/stream-handler';
import { installCodeCopyHandler } from './services/code-copy';
import { switchLease } from './services/lease';
import { registerDialogs } from './components/dialogs/impl';
import { initViewportHeight } from './core/viewport';
import { installFailsafe } from './core/failsafe';
import { syncTitle, initTitleAttention } from './services/title';
import { bridge, setBridge } from './core/bridge';
import { log, setLogLevel, getLogLevel, type LogLevel } from './logger';
import type { DispatchContext } from './services/dispatch';
import type { ServerMessage } from './core/types';

export interface MountOptions {
  /** Mount point for the React tree. */
  root: HTMLElement;
  /** Initial log level (default info). */
  logLevel?: LogLevel;
}

/** Handle returned by mountChat — the page's hook for runtime reconfiguration. */
export interface MountHandle {
  connection: ConnectConnection;
  /** Teardown of the delegated code-copy listener (tests uninstall it). */
  disposeCodeCopy?: () => void;
}

/** Auto-register all wire handlers (table-driven). */
registerAllHandlers();

export function mountChat(opts: MountOptions): MountHandle {
  if (opts.logLevel) setLogLevel(opts.logLevel);
  log.info('mount (log level: ' + getLogLevel() + ')');

  const conn = new ConnectConnection();

  // ctx.state is a GETTER: zustand setState replaces the state object, so a
  // snapshot would go stale after the first set. The getter always reads the
  // live state.
  const ctx: DispatchContext = {
    get state() {
      return useFlux.getState();
    },
    conn,
    bridge,
  };

  const uncopy = installCodeCopyHandler(document.body);

  setBridge({
    send: (msg) => conn.send(msg),
    reconnect: () => conn.reconnect(),
  });

  conn.setStatusHandler((status) => {
    useFlux.setState({ connectionStatus: status });
    // The server-side rounds die with the connection — converge the
    // streaming flags and stale leases at DISCONNECT time, not reconnect-
    // open: the composer must not offer Stop for a round that no longer
    // exists, and a message typed during the outage must take the plain
    // queued-send path instead of the interrupt-send flow. The open handler
    // still runs the same reset (idempotent) for the first-connect path.
    if (status === 'disconnected' || status === 'failed') resetStreamingForReconnect();
  });

  conn.setOpenHandler(resetStreamingForReconnect);

  conn.setMessageHandler((msg: ServerMessage) => {
    dispatchMessage(msg, ctx);
  });

  // First-party dialogs (confirm / new-chat / question) — before connect so
  // an early question_required always has an implementation.
  registerDialogs();
  // Window-level capture of unexpected failures (unhandled rejections,
  // listener throws) — logged + one deduplicated toast; nothing outside
  // the React tree reported before this.
  installFailsafe();
  // Keyboard-safe viewport height (mobile) — plain listeners, before the
  // first paint so the root never starts at the wrong height.
  initViewportHeight();

  // Render FIRST, synchronously — see the module comment for why.
  try {
    const root = createRoot(opts.root);
    flushSync(() => root.render(<App />));
    log.info('render done');
  } catch (err: unknown) {
    log.error('render failed: ' + (err instanceof Error ? err.message : String(err)));
  }

  // Identity continuity (D-19): restore this tab's last active chat BEFORE
  // the chats list arrives (the auto-claim lands on it), and persist every
  // change afterwards. Safe only AFTER the render — see the module comment.
  const storedActive = readStoredActiveChat();
  if (storedActive) useFlux.setState({ activeChatId: storedActive });
  useFlux.subscribe((s, prev) => {
    if (s.activeChatId !== prev.activeChatId) storeActiveChat(s.activeChatId);
  });

  // Sidebar collapse is a device preference — persist every change.
  useFlux.subscribe((s, prev) => {
    if (s.sidebarOpen !== prev.sidebarOpen) storeSidebarOpen(s.sidebarOpen);
  });

  // The document title rides conversation state — written on TRANSITIONS
  // only (the streaming object changes per flag flip, never per delta),
  // plus once here for the restored chat. Background attention clears
  // when the page becomes visible again.
  initTitleAttention();
  syncTitle();
  useFlux.subscribe((s, prev) => {
    if (
      s.activeChatId !== prev.activeChatId ||
      s.streaming !== prev.streaming ||
      s.backgroundEvents !== prev.backgroundEvents ||
      s.chats !== prev.chats
    ) {
      syncTitle();
    }
  });

  // Switching chats is a lease handover (D-05/D-06): chat_close the old chat
  // (unsubscribe + release), claim the new one. switchToChat flips the
  // visible pane; the claim is optimistic — a chat_busy error is turned into
  // a read-only pane by handlers.ts. The cursor advances BEFORE the work so
  // a thrown switch is never replayed whole on the next change — a missed
  // claim self-heals via the chats handler's reopen check (loadedChatId
  // stays behind). DOM failures must never kill the mounted app or the
  // connection (guarded).
  let prevActiveId = '';
  const onActiveChange = (newId: string) => {
    if (newId === prevActiveId) return;
    const from = prevActiveId;
    prevActiveId = newId;
    try {
      switchLease(from, newId);
      switchToChat(newId);
    } catch (err: unknown) {
      log.error('chat switch failed: ' + (err instanceof Error ? err.message : String(err)));
    }
  };
  // @preact/signals subscribe() fires IMMEDIATELY on registration with the
  // current value — the restored active chat's pane must materialize here,
  // before the chats list arrives. Replicate that semantics explicitly.
  onActiveChange(useFlux.getState().activeChatId);
  useFlux.subscribe((s, prev) => {
    if (s.activeChatId !== prev.activeChatId) onActiveChange(s.activeChatId);
  });

  conn.connect();
  log.info('init complete');
  return { connection: conn, disposeCodeCopy: uncopy };
}
