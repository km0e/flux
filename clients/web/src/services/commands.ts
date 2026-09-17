/**
 * commands.ts — the command palette's action registry: ONE list of chats
 * + application actions, each delegating to the SAME service entry the
 * owning surface uses (startNewChatFlow, bridge.reconnect, addTerminalTab,
 * openSearch, cycleTheme…). The registry never reimplements a flow — a
 * new entry point means wiring an existing one.
 *
 * `selectChat` lives here too (the sidebar row and the palette share it):
 * an activeChatId change triggers switchLease in mount → chat_claim — the
 * single message carrying history snapshot + subscription + lease; a
 * busy error degrades to a read-only pane (handlers.ts sends the
 * follow-up chat_open). No standalone chat_open exists.
 *
 * Provides: CommandAction, listCommands, selectChat
 * Depends: core/state.ts, core/bridge.ts, services/new-chat.ts,
 *          services/theme.ts, services/transcript-search.ts
 */
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { startNewChatFlow } from './new-chat';
import { cycleTheme } from './theme';
import { openSearch, searchSupported } from './transcript-search';

export interface CommandAction {
  id: string;
  title: string;
  /** 'Chats' entries are generated per conversation; 'Actions' are the
   * application verbs. The palette lists Actions first (verbs are the
   * repeat visitors), chats after. */
  group: 'Chats' | 'Actions';
  /** Extra match text (workdirs, shortcut names) — never rendered. */
  keywords?: string;
  /** Right-aligned secondary line (the workdir a chat runs in). */
  hint?: string;
  /** The bound chord, when one exists — the shortcuts sheet reads THIS,
   * so the sheet can never drift from the registry. */
  shortcut?: string;
  run(): void;
  /** Visibility gate — evaluated per palette open, not at module load. */
  available?: () => boolean;
}

/** Switch the active conversation (the sidebar row's and the palette's
 * shared path). A selection on the mobile drawer implies "done
 * navigating" — close it so the conversation shows. */
export function selectChat(id: string): void {
  useFlux.setState({ activeChatId: id });
  if (window.innerWidth <= 767.5) {
    useFlux.setState({ sidebarOpen: false });
  }
}

/** A sheet row: chord + what it does. Registry rows carry the action's
 * own `shortcut`; the palette itself can't (an open palette can't list
 * itself as an action), so it joins as a static row. */
export interface ShortcutRow {
  keys: string;
  label: string;
}

/** Registry-derived shortcut rows (gates IGNORED — the sheet documents
 * the bindings, it is not live state). */
export function listShortcutRows(): ShortcutRow[] {
  const rows = ACTIONS.filter((a) => a.shortcut).map((a) => ({
    keys: a.shortcut as string,
    label: a.title,
  }));
  return [{ keys: 'Ctrl+K', label: 'Command palette' }, ...rows];
}

/** The registry snapshot for one palette open. */
export function listCommands(): CommandAction[] {
  const s = useFlux.getState();
  const chats: CommandAction[] = s.chats.map((c) => ({
    id: `chat:${c.id}`,
    title: c.name,
    group: 'Chats',
    keywords: c.workdir,
    hint: c.workdir,
    run: () => selectChat(c.id),
  }));
  return [
    ...ACTIONS.filter((a) => a.available?.() ?? true),
    ...chats,
  ];
}

const ACTIONS: CommandAction[] = [
  {
    id: 'action:new-chat',
    title: 'New chat',
    group: 'Actions',
    keywords: 'create conversation workdir',
    run: () => startNewChatFlow(),
  },
  {
    id: 'action:search',
    title: 'Search in conversation',
    group: 'Actions',
    shortcut: 'Ctrl+F',
    keywords: 'find',
    available: () => searchSupported() && !!useFlux.getState().activeChatId,
    run: () => openSearch(),
  },
  {
    id: 'action:new-terminal',
    title: 'New terminal',
    group: 'Actions',
    keywords: 'shell pty',
    available: () => !!useFlux.getState().activeChatId,
    run: () => {
      const cid = useFlux.getState().activeChatId;
      if (cid) useFlux.getState().addTerminalTab(cid);
    },
  },
  {
    id: 'action:toggle-sidebar',
    title: 'Toggle sidebar',
    group: 'Actions',
    shortcut: 'Ctrl+B',
    keywords: 'hide show',
    run: () => useFlux.setState({ sidebarOpen: !useFlux.getState().sidebarOpen }),
  },
  {
    id: 'action:toggle-dock',
    title: 'Toggle files & terminal dock',
    group: 'Actions',
    shortcut: 'Ctrl+J',
    keywords: 'preview panel',
    run: () => useFlux.setState({ dockOpen: !useFlux.getState().dockOpen }),
  },
  {
    id: 'action:theme',
    title: 'Cycle theme (auto → dark → light)',
    group: 'Actions',
    keywords: 'appearance dark light color',
    run: () => cycleTheme(),
  },
  {
    id: 'action:settings',
    title: 'Open settings',
    group: 'Actions',
    keywords: 'preferences options',
    run: () => useFlux.getState().setSettingsOpen(true),
  },
  {
    id: 'action:settings-providers',
    title: 'Settings: Providers',
    group: 'Actions',
    keywords: 'endpoint api key model registry',
    run: () => useFlux.setState({ settingsTab: 'providers', settingsOpen: true }),
  },
  {
    id: 'action:settings-mcp',
    title: 'Settings: MCP servers',
    group: 'Actions',
    keywords: 'tools bridge stdio http',
    run: () => useFlux.setState({ settingsTab: 'mcp', settingsOpen: true }),
  },
  {
    id: 'action:settings-skills',
    title: 'Settings: Skills',
    group: 'Actions',
    keywords: 'skills instruction packages',
    run: () => useFlux.setState({ settingsTab: 'skills', settingsOpen: true }),
  },
  {
    id: 'action:shortcuts',
    title: 'Keyboard shortcuts',
    group: 'Actions',
    keywords: 'keys help cheat sheet ?',
    run: () => useFlux.getState().setShortcutsOpen(true),
  },
  {
    id: 'action:reconnect',
    title: 'Reconnect to server',
    group: 'Actions',
    keywords: 'connection offline retry',
    available: () => {
      const c = useFlux.getState().connectionStatus;
      return c === 'disconnected' || c === 'failed';
    },
    run: () => bridge.reconnect(),
  },
];
