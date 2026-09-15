/**
 * state.ts — Global reactive application state (zustand).
 *
 * Components subscribe via selectors: `useFlux((s) => s.chats)`. Imperative
 * services (handlers/stream/panes — the streaming pipeline is framework-free)
 * read and write through `useFlux.getState()` — no React, no hooks. Actions
 * live on the store; derived per-chat pruning lives in deleteChat.
 *
 * Provides: useFlux (store + actions), Chat, UsageTotals, FileTabState
 * Depends: core/types.ts, core/prefs.ts
 */
import { create } from 'zustand';
import { newId } from '../lib/id';
import {
  readStoredSidebarOpen,
  readStoredSidebarWidth,
  readStoredPreviewWidth,
  SIDEBAR_DEFAULT_WIDTH,
  PREVIEW_DEFAULT_WIDTH,
} from './prefs';
import type {
  UsageInfo,
  ProviderModelInfo,
  ProviderSummary,
  McpServerSummary,
  McpNoticeEntry,
  SkillSummary,
  SavedModelInfo,
  RoundArtifact,
} from './types';

/** Cumulative token usage for one chat (compact ↑/↓/R/W footer language).
 * `contextTokens` mirrors the last round's prompt — the context the model
 * currently carries; the cached share (`cachedTokens`) reads as cache-hit. */
export interface UsageTotals {
  inTokens: number;
  outTokens: number;
  cachedTokens: number;
  contextTokens: number;
}

const EMPTY_TOTALS: UsageTotals = {
  inTokens: 0,
  outTokens: 0,
  cachedTokens: 0,
  contextTokens: 0,
};

/** Toast stack cap + id sequence (module-level, monotonic). */
const TOAST_CAP = 4;
let toastSeq = 0;
let noticeSeq = 0;

export interface Chat {
  id: string;
  name: string;
  createdAt: number;
  /** Most recent message-append time (wire last_activity_at) — the sidebar's
   *  recency key. Optional so test fixtures omit it; consumers fall back to
   *  `createdAt`. */
  lastActivityAt?: number;
  /** Another window holds the lease (wire ChatInfo.active) — the sidebar shows "In use". */
  active: boolean;
  /** The chat's working directory (the tool sandbox boundary; shown as a cwd line). */
  workdir: string;
  /** The chat's pinned provider registry id. */
  provider: string;
  /** The chat's resolved model string. */
  model: string;
  /** Fork provenance — the SOURCE conversation (`undefined` = not a fork). */
  forked_from_chat_id?: string;
}

export type ConnectionStatus = 'connecting' | 'connected' | 'disconnected' | 'failed';

/** One file tab in the right dock: the `fs_read` result plus the UI
 * bookkeeping. The tab id IS the absolute path (dedupe by path). The
 * action buttons (Copy / Raw) live in the dock's tab strip — `rawView`
 * is the per-tab markdown render toggle they drive. */
export interface FileTabState {
  id: string;
  path: string;
  name: string;
  content: string;
  truncated: boolean;
  /** Full file size in bytes, when the server reported it. */
  size?: number;
  error?: string;
  loading: boolean;
  rawView: boolean;
}

/** One terminal tab: a client-side tab id (session key in
 * services/terminal.ts) bound to the chat whose workdir it runs in.
 * `seq` is the tab's creation order WITHIN its chat — the dock label
 * reads it, so closing a sibling never renumbers the survivors. */
export interface TerminalTabMeta {
  id: string;
  chatId: string;
  seq: number;
}

/** One unified-notification entry (the toast stack). Errors are sticky
 * (manual close); info auto-dismisses. Push dedupes by kind+text so a
 * repeated failure (e.g. the Explorer's refresh tick hitting the same
 * broken directory) refreshes one toast instead of spamming a stack. */
export interface ToastEntry {
  id: number;
  kind: 'error' | 'info';
  text: string;
}

export interface FluxStore {
  chats: Chat[];
  activeChatId: string;
  /** Registered providers (provider_list reply) for the pickers. */
  providers: ProviderSummary[];
  /** Registered MCP servers (mcp_list reply) for the MCP dialog. */
  mcpServers: McpServerSummary[];
  /** Installed skills (skills_list reply) for the Skills dialog. */
  skills: SkillSummary[];
  /** Probed upstream model catalogs, keyed by provider id (provider_models
   * replies; a probe failure caches []). */
  providerModels: Record<string, ProviderModelInfo[]>;
  /** Per-provider probe failures (the Providers dialog renders them with a
   * retry). Key absent = the last probe succeeded (or none ran yet). */
  providerProbeErrors: Record<string, string>;
  /** LOCAL saved models (models frame — the server's model registry with
   * editable params + models.dev metadata). The pickers read this FIRST,
   * then the probed catalogs. */
  savedModels: SavedModelInfo[];
  connectionStatus: ConnectionStatus;
  /** Cumulative per-chat token usage (compact ↑/↓/R/W totals). */
  usage: Record<string, UsageTotals>;
  /** Whether a chat is currently streaming (for cancel buttons). */
  streaming: Record<string, boolean>;
  /** Scroll-to-bottom floating button visibility. */
  scrollBtnVisible: boolean;
  /** The chat whose history was last loaded (chat_history). Guards against
   * double chat_claim sends and re-renders on refresh pushes. */
  loadedChatId: string;
  /** Chats being watched read-only (another window holds the lease →
   * chat_busy). While set, the composer is replaced by a viewer bar. */
  readonlyChats: Record<string, boolean>;
  /** The lease handover in flight (set by switchLease): the row we LEFT
   * still carries a stale `active` flag until the server's chats broadcast
   * lands — without this, the In-use badge flickers onto it for a frame.
   * Cleared by the next chats frame (setChats) or a short timeout. */
  leaseSwitch: { from: string; to: string } | null;
  /** Unified non-blocking notifications (errors from the filesystem
   * surfaces: Explorer listings, file reads). Capped, deduped. */
  toasts: ToastEntry[];
  sidebarOpen: boolean;
  sidebarWidth: number;
  previewWidth: number;
  /** Whether the right dock is visible at all. Closing it keeps the
   * open tabs (files + terminal) — reopening restores the last view. */
  dockOpen: boolean;
  /** Open file tabs (Explorer clicks; multi-file, like an editor).
   * Independent of the active chat — fs_read is chat-independent. */
  openFiles: FileTabState[];
  /** Terminal tabs (one session each; bound to a chat). Created via the
   * dock's "+" or the sidebar button — never auto-spawned. */
  terminalTabs: TerminalTabMeta[];
  /** The active dock tab id: a file tab id, a terminal tab id, or the
   * round-artifacts tab ('round'). */
  activeDockTab: string | null;
  /** The current round's artifacts per chat (F-11) — since the chat's
   * last user message, rebuilt from the history snapshot on re-open.
   * Written by services/artifacts.ts; the dock's Round tab reads it. */
  roundArtifacts: Record<string, RoundArtifact[]>;
  /** Forwarded MCP server notices (F-10b) — newest first, capped at 100.
   * Fire-and-forget session-level status, never conversation truth. */
  mcpNotices: McpNoticeEntry[];
  /** Notices arrived since the bell was last opened. */
  mcpNoticesUnread: number;
  /** Background attention for the document title ("while you were away"):
   * a round finishing or a question arriving in a NON-active chat while
   * the tab was hidden. Cleared when the page becomes visible again. */
  backgroundEvents: number;

  // ── Actions ──

  /** Fold one round's `usage` event into the chat's running totals. */
  addUsage(chatId: string, u: UsageInfo): void;
  /** Round-level truth for one chat (the scroll button converges on the
   * scroll listener + the MessageList streaming subscription — never here:
   * a store action must not reach into the DOM). */
  setStreaming(chatId: string, v: boolean): void;
  clearStreaming(chatId: string): void;
  setReadOnly(chatId: string, v: boolean): void;
  /** The authoritative chat list — switch focus if the active chat vanished. */
  setChats(chats: Chat[]): void;
  /** The provider-swap apply point — update one chat's provider/model. */
  updateChatProvider(chatId: string, provider: string, model: string): void;
  addChat(chat: Chat): void;
  renameChat(id: string, name: string): void;
  /** Unified notification: dedupe by kind+text, cap the stack. */
  pushToast(kind: ToastEntry['kind'], text: string): void;
  /** Fold one MCP server notice into the ring (newest first, capped)
   * and count it unread. */
  pushMcpNotice(server_id: string, level: string, message: string): void;
  /** The bell was opened — clear the unread counter. */
  markMcpNoticesRead(): void;
  /** "While you were away" attention bump for the document title. Counts
   * ONLY hidden-tab events — a visible window sees panes/toasts already. */
  bumpBackgroundEvents(): void;
  /** The page is visible again — clear the title attention counter. */
  clearBackgroundEvents(): void;
  dismissToast(id: number): void;
  /** Delete a chat + prune every per-chat record. */
  deleteChat(id: string): void;
  /** Bulk patch for services (no selectors needed outside React). */
  set(partial: Partial<FluxStore>): void;
  /** Open (or activate) a file tab in the right dock. */
  addFileTab(tab: FileTabState): void;
  /** Append a terminal tab for `chatId` and activate it; returns the id. */
  addTerminalTab(chatId: string, openDock?: boolean): string;
  /** Remove a terminal tab's meta (the session kill is the service's job). */
  removeTerminalTab(id: string): void;
  /** Patch one file tab in place (fetch result, loading flag). */
  patchFileTab(id: string, patch: Partial<FileTabState>): void;
  /** Close one file tab; the dock stays open (terminal may live there). */
  closeFileTab(id: string): void;
  setActiveDockTab(id: string): void;
  setDockOpen(open: boolean): void;
}

export const useFlux = create<FluxStore>()((set, get) => ({
  chats: [],
  activeChatId: '',
  providers: [],
  mcpServers: [],
  skills: [],
  providerModels: {},
  providerProbeErrors: {},
  savedModels: [],
  leaseSwitch: null,
  connectionStatus: 'disconnected',
  usage: {},
  streaming: {},
  scrollBtnVisible: false,
  loadedChatId: '',
  readonlyChats: {},
  toasts: [],
  sidebarOpen:
    readStoredSidebarOpen() ??
    // The one breakpoint (app.css matches ≤767.5px): a phone starts
    // collapsed — the drawer opens on demand. A tablet/laptop starts open.
    (typeof window !== 'undefined' && window.innerWidth <= 767.5 ? false : true),
  sidebarWidth: readStoredSidebarWidth() ?? SIDEBAR_DEFAULT_WIDTH,
  previewWidth: readStoredPreviewWidth() ?? PREVIEW_DEFAULT_WIDTH,
  dockOpen: false,
  openFiles: [],
  terminalTabs: [],
  activeDockTab: null,
  roundArtifacts: {},
  mcpNotices: [],
  mcpNoticesUnread: 0,
  backgroundEvents: 0,

  addUsage(chatId, u) {
    const prev = get().usage[chatId] ?? EMPTY_TOTALS;
    set({
      usage: {
        ...get().usage,
        [chatId]: {
          inTokens: prev.inTokens + u.prompt_tokens,
          outTokens: prev.outTokens + u.completion_tokens,
          cachedTokens: prev.cachedTokens + u.cached_tokens,
          contextTokens: u.prompt_tokens,
        },
      },
    });
  },

  setStreaming(chatId, v) {
    // Idempotence gate: the text-delta path calls this on EVERY delta —
    // the record rebuild + subscriber notifications must pay once per
    // transition, not once per delta (the boolean itself never changes).
    if (get().streaming[chatId] === v) return;
    set({ streaming: { ...get().streaming, [chatId]: v } });
  },

  clearStreaming(chatId) {
    const next = { ...get().streaming };
    delete next[chatId];
    set({ streaming: next });
  },

  setReadOnly(chatId, v) {
    const next = { ...get().readonlyChats };
    if (v) next[chatId] = true;
    else delete next[chatId];
    set({ readonlyChats: next });
  },

  setChats(chats) {
    const cur = get();
    let activeChatId = cur.activeChatId;
    // If the active chat was deleted, switch to the first remaining chat.
    if (activeChatId && !chats.some((c) => c.id === activeChatId)) {
      activeChatId = chats[0]?.id ?? '';
    }
    if (!activeChatId && chats.length > 0) {
      activeChatId = chats[0].id;
    }
    // Most-recent-ACTIVITY first — a DETERMINISTIC order. The wire's chat
    // list arrives in the server's HashMap iteration order (randomized per
    // process and reshuffled by inserts), so an unsorted list reorders rows
    // whenever a chat is created or the server restarts. Matches addChat's
    // unshift; createdAt is the fallback for fixtures without the stamp.
    chats.sort(
      (a, b) => (b.lastActivityAt ?? b.createdAt) - (a.lastActivityAt ?? a.createdAt),
    );
    // A chats frame is the release confirmation for an in-flight lease
    // switch — but ONLY a frame that actually carries the release (the
    // from-side row gone or freed). An unrelated broadcast that predates
    // the release — the fork's attach frame still shows the source
    // leased — must NOT end the window early, or the left row flashes
    // In-use until the real confirmation lands. The timeout fallback in
    // switchLease covers switches that release nothing.
    const sw = cur.leaseSwitch;
    const fromConfirmed =
      !sw || !chats.some((c) => c.id === sw.from && c.active);
    set({ chats, activeChatId, leaseSwitch: fromConfirmed ? null : cur.leaseSwitch });
  },

  addChat(chat) {
    set({
      chats: [chat, ...get().chats.filter((c) => c.id !== chat.id)],
      activeChatId: chat.id,
    });
  },

  renameChat(id, name) {
    set({ chats: get().chats.map((c) => (c.id === id ? { ...c, name } : c)) });
  },

  updateChatProvider(chatId, provider, model) {
    set({
      chats: get().chats.map((c) => (c.id === chatId ? { ...c, provider, model } : c)),
    });
  },

  pushMcpNotice(server_id, level, message) {
    const entry: McpNoticeEntry = {
      id: ++noticeSeq,
      server_id,
      level,
      message,
      at: Date.now(),
    };
    set((s) => ({
      mcpNotices: [entry, ...s.mcpNotices].slice(0, 100),
      mcpNoticesUnread: s.mcpNoticesUnread + 1,
    }));
  },

  markMcpNoticesRead() {
    set({ mcpNoticesUnread: 0 });
  },

  bumpBackgroundEvents() {
    // "While you were away" semantics: only events landing while the tab
    // is hidden count.
    if (!document.hidden) return;
    set((s) => ({ backgroundEvents: s.backgroundEvents + 1 }));
  },

  clearBackgroundEvents() {
    set({ backgroundEvents: 0 });
  },

  pushToast(kind, text) {
    const prev = get().toasts;
    const deduped = prev.filter((t) => !(t.kind === kind && t.text === text));
    const entry: ToastEntry = { id: ++toastSeq, kind, text };
    // Cap the stack — the oldest entry (possibly the deduped survivor's
    // predecessor) yields to the fresh one.
    set({ toasts: [...deduped, entry].slice(-TOAST_CAP) });
  },

  dismissToast(id) {
    set({ toasts: get().toasts.filter((t) => t.id !== id) });
  },

  deleteChat(id) {
    const cur = get();
    const chats = cur.chats.filter((c) => c.id !== id);
    const patch: Partial<FluxStore> = { chats };
    if (cur.activeChatId === id) patch.activeChatId = chats[0]?.id ?? '';
    if (cur.loadedChatId === id) patch.loadedChatId = '';
    const usage = { ...cur.usage };
    delete usage[id];
    patch.usage = usage;
    const streaming = { ...cur.streaming };
    delete streaming[id];
    patch.streaming = streaming;
    const readonlyChats = { ...cur.readonlyChats };
    delete readonlyChats[id];
    patch.readonlyChats = readonlyChats;
    set(patch);
  },

  set(partial) {
    set(partial);
  },

  addFileTab(tab) {
    const openFiles = get().openFiles;
    const existing = openFiles.find((t) => t.id === tab.id);
    set({
      dockOpen: true,
      activeDockTab: tab.id,
      openFiles: existing ? openFiles : [...openFiles, tab],
    });
  },

  patchFileTab(id, patch) {
    set({
      openFiles: get().openFiles.map((t) => (t.id === id ? { ...t, ...patch } : t)),
    });
  },

  closeFileTab(id) {
    const openFiles = get().openFiles.filter((t) => t.id !== id);
    const patch: Partial<FluxStore> = { openFiles };
    if (get().activeDockTab === id) {
      // Fall back to any other open tab (terminal tabs included).
      patch.activeDockTab = openFiles.at(-1)?.id ?? get().terminalTabs.at(-1)?.id ?? null;
    }
    set(patch);
  },

  addTerminalTab(chatId, openDock = true) {
    // newId, NOT crypto.randomUUID — that API is secure-context-only and
    // the server is routinely reached over plain HTTP from a LAN address.
    const id = `term-${newId()}`;
    // The label number is the creation order within the chat — computed
    // ONCE here, not from a live index (an index shifts when a sibling
    // tab closes, renaming "Terminal 2" into "Terminal 1").
    const seq =
      Math.max(
        0,
        ...get()
          .terminalTabs.filter((t) => t.chatId === chatId)
          .map((t) => t.seq),
      ) + 1;
    // `openDock=false` is the refresh-restore path: tabs come back
    // silently — the user's closed dock must not pop open over them.
    set({
      ...(openDock ? { dockOpen: true, activeDockTab: id } : {}),
      terminalTabs: [...get().terminalTabs, { id, chatId, seq }],
    });
    return id;
  },

  removeTerminalTab(id) {
    const terminalTabs = get().terminalTabs.filter((t) => t.id !== id);
    const patch: Partial<FluxStore> = { terminalTabs };
    if (get().activeDockTab === id) {
      patch.activeDockTab = get().openFiles.at(-1)?.id ?? terminalTabs.at(-1)?.id ?? null;
    }
    set(patch);
  },

  setActiveDockTab(id) {
    set({ activeDockTab: id, dockOpen: true });
  },

  setDockOpen(open) {
    set({ dockOpen: open });
  },
}));

/** Reset every field to its initial value (test isolation). */
export function resetFluxForTest(): void {
  useFlux.setState({
    chats: [],
    activeChatId: '',
    providers: [],
    mcpServers: [],
    skills: [],
    providerModels: {},
    providerProbeErrors: {},
    savedModels: [],
    leaseSwitch: null,
    connectionStatus: 'disconnected',
    usage: {},
    streaming: {},
    scrollBtnVisible: false,
    loadedChatId: '',
    readonlyChats: {},
    toasts: [],
    sidebarOpen: true,
    sidebarWidth: SIDEBAR_DEFAULT_WIDTH,
    previewWidth: PREVIEW_DEFAULT_WIDTH,
    dockOpen: false,
    openFiles: [],
    terminalTabs: [],
    activeDockTab: null,
    roundArtifacts: {},
    mcpNotices: [],
    mcpNoticesUnread: 0,
    backgroundEvents: 0,
  });
}
