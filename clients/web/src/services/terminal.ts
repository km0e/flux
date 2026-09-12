/**
 * terminal.ts — terminal sessions over the `/ws/term` side channel
 * (e4pty PTY on the server, xterm.js here).
 *
 * One session per TERMINAL TAB (the dock's "+" and the sidebar button
 * create tabs on demand — never auto-spawned); each session is bound to
 * the chat whose workdir it runs in and keeps running in the background:
 * output appends to the xterm buffer while the tab is unmounted.
 *
 * Session identity reuse: server-assigned terminal ids are remembered
 * per chat in sessionStorage, so a page refresh restores the tabs and
 * re-attaches to the SAME PTYs within the session grace window (the
 * server replays its scrollback ring after the hello — output written
 * while detached rebuilds on the client).
 *
 * The socket is SELF-HEALING: every unexpected end (backend restart,
 * network blip, reattach rejection) re-enters a capped-backoff retry
 * chain. A stale term id after a backend restart falls back to a fresh
 * spawn server-side, so retrying always converges once the main WS has
 * re-established the session identity.
 *
 * Frames: binary = raw terminal bytes both ways; text = JSON control
 * frames (hello / resize / exited / close / error).
 *
 * Provides: createTerminal, restoreTerminals, killTerminal, pruneSessions,
 *           terminalSession, fitTerminal, watchResize, TermSession
 * Depends: core/session.ts, core/state.ts
 */
import { useFlux } from '../core/state';
import { readStoredSessionId } from '../core/session';

/** Where a chat's terminal ids survive refreshes (session identity reuse). */
const TERM_KEY_PREFIX = 'flux.terms.';

export type TermStatus = 'connecting' | 'running' | 'exited' | 'failed';

export interface TermSession {
  tabId: string;
  chatId: string;
  /** The server-assigned terminal id (reattach handle across refreshes). */
  id: string | null;
  ws: WebSocket | null;
  xterm: import('@xterm/xterm').Terminal | null;
  fit: import('@xterm/addon-fit').FitAddon | null;
  /** Persistent DOM node — moved between mounts by TerminalPanel. */
  container: HTMLDivElement | null;
  /** Whether xterm.open() has run on an ATTACHED container. */
  opened: boolean;
  /** Resolves once the lazy chunks loaded and the terminal is usable
   *  (the socket lifecycle is independent — see connect). */
  ready: Promise<void>;
  status: TermStatus;
  exitedCode: number | null;
  resizeObserver: ResizeObserver | null;
  /** The user (or a prune) closed this tab — the retry chain must die. */
  killed: boolean;
  retryAttempt: number;
  retryTimer: number | null;
}

const sessions = new Map<string, TermSession>();

/** Reconnect backoff: 1s → 2s → 4s → 5s cap. A backend restart empties
 * the terminal registry and kills the PTYs; the reattach-with-stale-id
 * path falls back to a FRESH SPAWN server-side, so retrying always
 * converges once the main WS has re-established the session identity. */
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 5000;

function reconnectDelay(attempt: number): number {
  return Math.min(RECONNECT_MAX_MS, RECONNECT_BASE_MS * 2 ** Math.min(attempt, 3));
}

/** The single retry authority — every unexpected socket end lands here
 * (socket close, connect failure). NOT for killed tabs (the user asked
 * for them to die) or exited shells (the PTY is gone; a respawn would be
 * a new shell nobody asked for). */
function handleDisconnect(session: TermSession): void {
  if (session.killed || session.status === 'exited') return;
  if (session.retryTimer !== null) window.clearTimeout(session.retryTimer);
  session.status = 'connecting';
  session.retryAttempt += 1;
  session.retryTimer = window.setTimeout(
    () => {
      session.retryTimer = null;
      connect(session);
    },
    reconnectDelay(session.retryAttempt),
  );
}

/** Chats whose tabs were already restored after a refresh — the App-level
 * restore effect refires on reconnects; without this guard each refire
 * would duplicate the tabs (all reattaching to the same PTYs). */
const restoredChats = new Set<string>();

/** The live session for a dock tab, if any. */
export function terminalSession(tabId: string): TermSession | undefined {
  return sessions.get(tabId);
}

function readSavedIds(chatId: string): string[] {
  try {
    const raw = sessionStorage.getItem(TERM_KEY_PREFIX + chatId);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((x): x is string => typeof x === 'string') : [];
  } catch {
    return [];
  }
}

function saveIds(chatId: string, ids: string[]): void {
  try {
    sessionStorage.setItem(TERM_KEY_PREFIX + chatId, JSON.stringify(ids));
  } catch {
    // storage unavailable — refresh simply loses the reattach handles
  }
}

/** Keep the chat's saved list in sync with the live sessions. */
function syncSaved(chatId: string): void {
  const ids = [...sessions.values()]
    .filter((s) => s.chatId === chatId && s.id)
    .map((s) => s.id as string);
  saveIds(chatId, ids);
}

/** The terminal font stack — the bundled faces first (see
 * styles/fonts.css), system monos as last-ditch fallback. The base face
 * is JetBrains Mono (readable at small sizes, box-drawing + powerline
 * glyphs built in); icon codepoints fall through to the bundled Nerd Font
 * Mono face — or a local Nerd/Symbols font when one is installed. */
const FONT_STACK =
  "'JetBrains Mono', 'JetBrains Mono NF', ui-monospace, 'Cascadia Mono', Menlo, Consolas, monospace";

/** The terminal palette from the --fx-* tokens (theme-aware). ANSI-16
 * stays xterm's default — the tokens carry no 16-color scale. */
function currentTheme(): import('@xterm/xterm').ITheme {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  return {
    background: v('--fx-inset', '#0a0c10'),
    foreground: v('--fx-fg', '#e7eaf0'),
    cursor: v('--fx-accent', '#8b87f7'),
    cursorAccent: v('--fx-bg', '#0e1015'),
    selectionBackground: v('--fx-selection', 'rgba(139,135,247,0.18)'),
  };
}

/** Re-read the tokens into every live session (theme-toggle hook). */
export function applyTerminalTheme(): void {
  const theme = currentTheme();
  for (const s of sessions.values()) {
    if (s.xterm) s.xterm.options.theme = theme;
  }
}

/** Create a terminal tab (store meta) AND its session — the explicit
 * user ask ("add a terminal"), never called automatically. */
export async function createTerminal(chatId: string): Promise<string | null> {
  if (!chatId) return null;
  const tabId = useFlux.getState().addTerminalTab(chatId);
  spawnSession(tabId, chatId, null);
  return tabId;
}

/** Restore a chat's terminal tabs after a refresh: one tab per saved
 * server id, each re-attaching to the still-alive PTY. Called once per
 * chat when its identity is confirmed. */
export async function restoreTerminals(chatId: string): Promise<void> {
  if (!chatId || restoredChats.has(chatId)) return;
  restoredChats.add(chatId);
  for (const savedId of readSavedIds(chatId)) {
    // Silent restore: the user may have closed the dock before refreshing —
    // reopening it over their head is wrong. The tabs are back; the dock
    // opens when the user asks for it (and the PTY output keeps buffering
    // server-side either way — see the scrollback replay).
    const tabId = useFlux.getState().addTerminalTab(chatId, false);
    spawnSession(tabId, chatId, savedId);
  }
}

/** Spawn the PTY-backed session for a tab: lazy chunks → xterm + container
 * → socket → hello. The xterm.open() happens at MOUNT time (mountSession)
 * — the container must be attached and sized first. */
function spawnSession(tabId: string, chatId: string, reattachId: string | null): void {
  if (sessions.has(tabId)) return;
  const session: TermSession = {
    tabId,
    chatId,
    id: reattachId,
    ws: null,
    xterm: null,
    fit: null,
    container: null,
    opened: false,
    ready: Promise.resolve(),
    status: 'connecting',
    exitedCode: null,
    resizeObserver: null,
    killed: false,
    retryAttempt: 0,
    retryTimer: null,
  };
  sessions.set(tabId, session);
  session.ready = (async () => {
    // Lazy chunks: xterm + fit + the font faces load on the FIRST terminal
    // creation only.
    const [{ Terminal }, { FitAddon }] = await Promise.all([
      import('@xterm/xterm'),
      import('@xterm/addon-fit'),
    ]);
    await import('@xterm/xterm/css/xterm.css');
    await import('../styles/fonts.css');

    const xterm = new Terminal({
      fontFamily: FONT_STACK,
      fontSize: 13,
      cursorBlink: true,
      scrollback: 5000,
      theme: currentTheme(),
    });
    const fit = new FitAddon();
    xterm.loadAddon(fit);
    const container = document.createElement('div');
    container.className = 'absolute inset-0 px-1.5 py-1';
    session.xterm = xterm;
    session.fit = fit;
    session.container = container;

    // Keystrokes ride BINARY frames (text frames are control frames only).
    xterm.onData((data) => {
      if (session.ws?.readyState === WebSocket.OPEN) {
        session.ws.send(new TextEncoder().encode(data));
      }
    });

    // The socket lifecycle is SELF-HEALING and independent of the mount:
    // connect → hello, with backoff retries on any unexpected end. The
    // session is usable (mountable) the moment the chunks land — the
    // socket arrives whenever it arrives.
    connect(session);
  })();
}

/** Attach the session's container to `host` and open xterm there (once).
 * `cancelled` lets a stale caller abort after the async readiness wait —
 * without it a fast unmount would still append into a detached host. */
export async function mountSession(
  tabId: string,
  host: HTMLElement,
  cancelled?: () => boolean,
): Promise<void> {
  const s = sessions.get(tabId);
  if (!s) return;
  await s.ready.catch(() => {});
  if (cancelled?.()) return;
  if (!s.container || !s.xterm) return;
  if (s.container.parentElement !== host) host.appendChild(s.container);
  if (!s.opened) {
    try {
      s.xterm.open(s.container);
    } catch {
      // No render surface (jsdom tests, detached box) — the status shows
      // the failure; nothing may escape as an unhandled rejection.
      s.status = 'failed';
      return;
    }
    s.opened = true;
    s.resizeObserver = new ResizeObserver(() => fitTerminal(tabId));
    s.resizeObserver.observe(s.container);
    // Double-rAF: the container must be laid out before the first fit.
    requestAnimationFrame(() => requestAnimationFrame(() => fitTerminal(tabId)));
    // The canvas renderer measures char cells ONCE at open — with the web
    // fonts still loading it would draw (and fit) the fallback metrics.
    // Wait for the faces, then force a re-measure + redraw by reassigning
    // the family. Both faces are loaded up front: the canvas atlas has no
    // per-glyph re-flow, so a lazy icon face would leave tofu behind.
    void (async () => {
      try {
        if (typeof document !== 'undefined' && document.fonts) {
          const size = `${s.xterm!.options.fontSize ?? 13}px`;
          await Promise.race([
            Promise.all([
              document.fonts.load(`${size} "JetBrains Mono"`),
              document.fonts.load(`${size} "JetBrains Mono NF"`),
            ]),
            new Promise((r) => setTimeout(r, 3000)),
          ]);
        }
      } catch {
        /* no FontFaceSet (jsdom) — the fallback stack stays */
      }
      if (cancelled?.() || !s.opened || !s.xterm) return;
      s.xterm.options.fontFamily = FONT_STACK;
      fitTerminal(tabId);
    })();
  }
}

/** (Re)connect the session's socket — fresh spawn or reattach by the
 * saved server id. VOID by design: the socket lifecycle is self-healing
 * and must never block the mount. Every unexpected end (no hello in 8s,
 * backend restart, reattach rejection — the server answers a stale term
 * id by falling back to a fresh spawn) re-enters the retry chain through
 * onclose → handleDisconnect. */
function connect(session: TermSession): void {
  if (session.killed) return;
  const token = readStoredSessionId();
  if (!token) {
    // No identity yet (the main WS is still re-establishing it) — back
    // off and retry; the token reappears once the resume lands.
    handleDisconnect(session);
    return;
  }
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(`${proto}//${location.host}/ws/term`);
  ws.binaryType = 'arraybuffer';
  session.ws = ws;
  session.status = 'connecting';
  session.exitedCode = null;

  ws.onopen = () => {
    // The handshake: the FIRST frame is the auth frame — the session token
    // is a bearer credential and never rides the URL (query params land in
    // access/proxy logs). The server answers hello or error+close.
    ws.send(
      JSON.stringify({
        type: 'auth',
        session: token,
        chat: session.chatId,
        ...(session.id ? { term: session.id } : {}),
      }),
    );
  };

  const timer = window.setTimeout(() => {
    // No hello within 8s — dead weight; closing fires onclose, which
    // owns the retry.
    try {
      ws.close();
    } catch {
      /* already closing */
    }
  }, 8000);

  ws.onmessage = (ev) => {
    if (typeof ev.data === 'string') {
      const frame = JSON.parse(ev.data) as Record<string, unknown>;
      switch (frame.type) {
        case 'hello': {
          window.clearTimeout(timer);
          session.id = String(frame.term);
          session.status = 'running';
          session.retryAttempt = 0;
          syncSaved(session.chatId);
          fitTerminal(session.tabId);
          return;
        }
        case 'exited': {
          session.status = 'exited';
          session.exitedCode = Number(frame.code ?? -1);
          // Clean exit closes the tab: the server has already torn down
          // the PTY (and the entry), and a code-0 shell exit is a
          // deliberate user act — the tab is dead UI. A FAILED shell
          // (non-zero) stays open with its exit-code status line for
          // diagnosis.
          if (session.exitedCode === 0) {
            teardownSession(session);
            return;
          }
          void session.xterm?.write('\r\n');
          return;
        }
        default:
          return;
      }
    }
    // Raw PTY output (ArrayBuffer — binaryType set above).
    void session.xterm?.write(new Uint8Array(ev.data as ArrayBuffer));
  };
  ws.onclose = () => {
    window.clearTimeout(timer);
    // The server keeps a live PTY past a socket close (grace reattach);
    // this end may equally be a backend restart. Either way the retry
    // chain re-enters: a live PTY reattaches (scrollback replayed), a
    // stale id falls back to a fresh spawn. handleDisconnect refuses the
    // two terminal states nobody wants revived: killed tabs, exited
    // shells.
    handleDisconnect(session);
  };
  ws.onerror = () => {
    // onclose always follows an error — the retry decision lives there
    // (the connect timeout is cleared on this same path).
  };
}

/** Recompute the fit and propagate the geometry upstream. Safe to call
 * repeatedly (ResizeObserver, tab activation, window resize). */
export function fitTerminal(tabId: string): void {
  const s = sessions.get(tabId);
  if (!s?.xterm || !s.fit || !s.container?.isConnected) return;
  if (s.container.clientWidth === 0 || s.container.clientHeight === 0) return;
  try {
    s.fit.fit();
    const { rows, cols } = s.xterm;
    if (s.ws?.readyState === WebSocket.OPEN && cols > 0 && rows > 0) {
      s.ws.send(JSON.stringify({ type: 'resize', cols, rows }));
    }
  } catch {
    // fit on a degenerate box — the next resize retries
  }
}

/** Observe a mounted host for geometry changes (dock drag, window). */
export function watchResize(tabId: string, host: HTMLElement): void {
  const s = sessions.get(tabId);
  if (!s) return;
  const ro = new ResizeObserver(() => fitTerminal(tabId));
  ro.observe(host);
  // The container-level observer (set at spawn) keeps working elsewhere.
  void s.resizeObserver;
}

/** Kill the PTY (server `close` frame), dispose everything, drop the
 * session + the tab meta. */
export function killTerminal(tabId: string): void {
  const s = sessions.get(tabId);
  if (!s) return;
  // Mark FIRST: the socket close must not re-enter the retry chain — the
  // user asked for this tab to die.
  s.killed = true;
  if (s.ws?.readyState === WebSocket.OPEN) {
    s.ws.send(JSON.stringify({ type: 'close' }));
  }
  teardownSession(s);
}

/** Local teardown shared by the user kill and the clean-exit auto-close:
 * stop any retry chain, dispose everything, drop the session + the tab
 * meta + the saved reattach id (a refresh must not restore a dead
 * terminal). The caller owns the socket decision — kill notifies the
 * server first; on the auto-close path the server already tore the PTY
 * down (the exited frame IS that notification). */
function teardownSession(s: TermSession): void {
  s.killed = true;
  if (s.retryTimer !== null) window.clearTimeout(s.retryTimer);
  s.retryTimer = null;
  s.ws?.close();
  s.resizeObserver?.disconnect();
  s.xterm?.dispose();
  s.container?.remove();
  sessions.delete(s.tabId);
  if (s.id) {
    saveIds(
      s.chatId,
      readSavedIds(s.chatId).filter((x) => x !== s.id),
    );
  }
  useFlux.getState().removeTerminalTab(s.tabId);
}

/** Close sessions whose chat is gone (chat deleted elsewhere) — the
 * server kills the PTYs anyway; this just stops the dead sockets and
 * drops the tab metas (the same teardown the user kill runs). */
export function pruneSessions(liveChatIds: Set<string>): void {
  for (const s of [...sessions.values()]) {
    if (!liveChatIds.has(s.chatId)) {
      teardownSession(s);
    }
  }
}
