/**
 * TopBar.tsx — the GLOBAL bar: app identity and app-wide status only.
 *
 * Brand mark → spacer → streaming indicator (click = cancel) → connection
 * (click = reconnect when down) → settings gear (opens the one tabbed
 * settings dialog DIRECTLY — the sections are tabs inside it, no menu) →
 * theme toggle. The CONVERSATION's identity (name, workdir, usage)
 * lives in ChatHeader — the bar above it never repeats chat state, so the
 * layout is stable and narrow viewports stay calm.
 *
 * Provides: TopBar
 * Depends: core/state.ts, core/bridge.ts, hooks/useTheme.ts,
 *          components/ui/*, component./settings/SettingsDialog
 */
import { lazy, Suspense, useState } from 'react';
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import type { ThemeChoice } from '../core/prefs';
import { useTheme } from '../hooks/useTheme';
import { Button, IconButton, Spinner } from './ui';
import { Tooltip } from './ui/tooltip';
import {
  Bell,
  Menu,
  Monitor,
  Moon,
  Settings,
  Sun,
  WifiOff,
  X,
} from 'lucide-react';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from './ui/dropdown-menu';
import { cn } from '../lib/cn';

const STATUS_LABEL: Record<string, string> = {
  connecting: 'Connecting',
  connected: 'Connected',
  disconnected: 'Disconnected',
  failed: 'Connection failed',
};

const THEME_ICON: Record<ThemeChoice, React.ReactNode> = {
  auto: <Monitor size={14} />,
  dark: <Moon size={14} />,
  light: <Sun size={14} />,
};

const THEME_LABEL: Record<ThemeChoice, string> = {
  auto: 'Auto (follow system)',
  dark: 'Dark',
  light: 'Light',
};

// The settings chain (dialog + the Providers/MCP/Skills panels, ~30 KB of
// app code) rides its own chunk and loads on first gear click — the entry
// stays first-party-only (see vite.config.ts's chunking comment). The
// module loads exactly once, so the last-visited-section memory (module
// state inside the dialog) survives close/reopen as before.
const SettingsDialog = lazy(() =>
  import('./settings/SettingsDialog').then((m) => ({ default: m.SettingsDialog })),
);

/** The MCP notice bell (F-10b): the notification center over the
 * rate-limited server notices. Session-level, fire-and-forget — a
 * page refresh clears the ring (the server keeps no history), which is
 * honest for transient status. Opening the menu marks it read. */
function McpNoticeBell(): React.ReactElement {
  const notices = useFlux((s) => s.mcpNotices);
  const unread = useFlux((s) => s.mcpNoticesUnread);
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <span className="relative inline-flex">
          <IconButton
            id="notice-bell"
            label={`Notifications${unread > 0 ? ` (${unread} unread)` : ''}`}
            className="size-7"
            onClick={() => useFlux.getState().markMcpNoticesRead()}
          >
            <Bell size={14} />
          </IconButton>
          {unread > 0 && (
            <span
              aria-hidden="true"
              className="pointer-events-none absolute -top-1 -right-1 grid h-4 min-w-4 place-items-center rounded-full bg-accent px-1 text-2xs leading-none font-bold text-accent-fg"
            >
              {unread > 9 ? '9+' : unread}
            </span>
          )}
        </span>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-80 max-md:w-[calc(100vw-2rem)]">
        <DropdownMenuItem disabled className="text-2xs text-faint">
          MCP server notices
        </DropdownMenuItem>
        {notices.length === 0 ? (
          <DropdownMenuItem disabled className="text-xs text-muted">
            Nothing yet — server logs land here when tools run.
          </DropdownMenuItem>
        ) : (
          <div className="max-h-80 overflow-y-auto">
            {notices.map((n) => (
              <DropdownMenuItem key={n.id} disabled className="flex-col items-start gap-0.5">
                <span className="flex w-full items-center gap-1.5">
                  <span
                    className={cn(
                      'inline-block size-1.5 shrink-0 rounded-full',
                      LOUD_LEVELS.has(n.level) ? 'bg-warn' : 'bg-faint',
                    )}
                    aria-hidden="true"
                  />
                  <span className="font-mono text-2xs text-muted">{n.server_id}</span>
                  <span className="text-2xs text-faint">{n.level}</span>
                  <span className="flex-1" />
                  <span className="text-2xs text-faint tabular-nums">
                    {new Date(n.at).toLocaleTimeString()}
                  </span>
                </span>
                <span className="line-clamp-2 w-full text-xs text-fg">{n.message}</span>
              </DropdownMenuItem>
            ))}
          </div>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

/** Levels that read as alerts (warn dot + error toast at arrival). */
const LOUD_LEVELS = new Set(['warning', 'error', 'critical', 'alert', 'emergency']);

export function TopBar(): React.ReactElement {
  const cid = useFlux((s) => s.activeChatId);
  const conn = useFlux((s) => s.connectionStatus);
  const streaming = useFlux((s) => (cid ? (s.streaming[cid] ?? false) : false));
  const sidebarOpen = useFlux((s) => s.sidebarOpen);
  const [theme, cycleTheme] = useTheme();
  // Settings: the gear opens the one tabbed dialog DIRECTLY (no menu —
  // the sections are tabs inside it). Reopening lands on the last-visited
  // section (the dialog keeps that memory itself across unmounts).
  const [settingsOpen, setSettingsOpen] = useState(false);
  const down = conn === 'disconnected' || conn === 'failed';
  const wsTarget = location.host;

  return (
    <div
      id="top-bar"
      role="banner"
      className="flex h-10 max-md:h-12 shrink-0 items-center gap-2 border-b border-border bg-panel px-2.5 max-md:pt-[env(safe-area-inset-top)]"
    >
      <IconButton
        id="sidebar-toggle"
        label={sidebarOpen ? 'Close sidebar' : 'Open sidebar'}
        aria-expanded={sidebarOpen}
        onClick={() => useFlux.setState({ sidebarOpen: !sidebarOpen })}
        className="size-7"
      >
        {/* The drawer regime (mobile) flips the glyph to an X while open —
            the drawer starts below this bar, so the X is its always-visible
            close control. Desktop keeps the stable Menu glyph (the sidebar
            is a pane, not a modal surface). CSS-only switch: layout never
            depends on the JS breakpoint. */}
        <Menu size={15} aria-hidden="true" className={cn(sidebarOpen && 'max-md:hidden')} />
        <X size={15} aria-hidden="true" className={cn('md:hidden', !sidebarOpen && 'hidden')} />
      </IconButton>

      {/* Brand — the app's identity. The conversation's identity lives in
          ChatHeader; this bar carries only what spans every chat. */}
      <img src="/assets/favicon.svg" alt="" aria-hidden="true" className="size-6 shrink-0" />
      <span className="text-sm font-semibold tracking-[-0.01em] max-sm:hidden">Flux</span>

      <span className="flex-1" />

      {/* Streaming indicator doubles as a cancel button. */}
      {streaming && cid && (
        <Tooltip content="Stop the current round (Esc)" side="bottom">
          <Button
            id="topbar-working"
            variant="ghost"
            size="sm"
            className="text-info"
            onClick={() => bridge.send({ type: 'cancel', chat_id: cid })}
          >
            <Spinner />
            working…
          </Button>
        </Tooltip>
      )}

      {/* Connection: a plain status when up; a verb when down (the state
          rides the color + tooltip, the label stays actionable). */}
      {down ? (
        <Tooltip content={`${STATUS_LABEL[conn]} — click to retry`} side="bottom">
          <Button
            id="topbar-conn"
            variant="ghost"
            size="sm"
            className="text-danger"
            onClick={() => bridge.reconnect()}
          >
            <WifiOff size={12} />
            Reconnect
          </Button>
        </Tooltip>
      ) : conn === 'connected' ? (
        <Tooltip content={`Connected to ${wsTarget}`} side="bottom">
          <span id="topbar-conn" className="inline-flex items-center gap-1.5 text-xs text-muted">
            <span className="size-2 rounded-full bg-success" />
            {/* The label reads on ≥640px; tiny screens keep the dot (the
                tooltip + the chat header carry the state). */}
            <span className="max-sm:hidden">Connected</span>
          </span>
        </Tooltip>
      ) : (
        <span id="topbar-conn" className="inline-flex items-center gap-1.5 text-xs text-muted">
          <span className={cn('size-2 animate-pulse rounded-full bg-warn')} />
          {conn === 'connecting' ? 'Connecting…' : STATUS_LABEL[conn]}
        </span>
      )}

      {/* Server-global settings — one gear, one dialog, three sections
          inside. The per-chat provider/model switch lives at the composer
          footer (ChatView). */}
      <McpNoticeBell />
      <IconButton
        id="settings-button"
        label="Settings"
        className="size-7"
        onClick={() => setSettingsOpen(true)}
      >
        <Settings size={14} />
      </IconButton>

      <Tooltip content={`Theme: ${THEME_LABEL[theme]} (click to cycle)`} side="bottom">
        <IconButton
          id="theme-toggle"
          label={`Theme: ${THEME_LABEL[theme]}`}
          onClick={cycleTheme}
          className="size-7"
        >
          {THEME_ICON[theme]}
        </IconButton>
      </Tooltip>

      {settingsOpen && (
        <Suspense
          fallback={
            <IconButton label="Settings" className="size-7" aria-busy="true">
              <Spinner />
            </IconButton>
          }
        >
          <SettingsDialog onClose={() => setSettingsOpen(false)} />
        </Suspense>
      )}
    </div>
  );
}
