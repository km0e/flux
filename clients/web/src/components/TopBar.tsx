/**
 * TopBar.tsx — the GLOBAL bar: app identity and app-wide status only.
 *
 * Brand mark → spacer → streaming indicator (click = cancel) → connection
 * (click = reconnect when down) → settings gear (opens the one tabbed
 * settings dialog DIRECTLY — the sections are tabs inside it, no menu) →
 * theme toggle. The CONVERSATION's identity (name, kind, workdir, usage)
 * lives in ChatHeader — the bar above it never repeats chat state, so the
 * layout is stable and narrow viewports stay calm.
 *
 * Provides: TopBar, useTheme
 * Depends: core/state.ts, core/bridge.ts, core/prefs.ts, components/ui/*,
 *          components/dialogs/SettingsDialog
 */
import { useState } from 'react';
import { useFlux } from '../core/state';
import { bridge } from '../core/bridge';
import { readStoredTheme, storeTheme, type ThemeChoice } from '../core/prefs';
import { Button, IconButton, Spinner } from './ui';
import { SettingsDialog } from './dialogs/SettingsDialog';
import { Tooltip } from './ui/tooltip';
import {
  Menu,
  Monitor,
  Moon,
  Settings,
  Sun,
  WifiOff,
} from 'lucide-react';
import { cn } from '../lib/cn';

const STATUS_LABEL: Record<string, string> = {
  connecting: 'Connecting',
  connected: 'Connected',
  disconnected: 'Disconnected',
  failed: 'Connection failed',
};

const NEXT_THEME: Record<ThemeChoice, ThemeChoice> = {
  auto: 'dark',
  dark: 'light',
  light: 'auto',
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

/** Cycle the theme choice; explicit values pin the color-scheme. */
export function useTheme(): [ThemeChoice, () => void] {
  const [theme, setTheme] = useState<ThemeChoice>(() => readStoredTheme() ?? 'auto');
  const cycle = () => {
    const next = NEXT_THEME[theme];
    setTheme(next);
    storeTheme(next);
    // The inline bootstrap in index.html handled the initial state; explicit
    // choices pin the attribute, auto clears it (the media query owns it).
    if (next === 'auto') delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = next;
  };
  return [theme, cycle];
}

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
        <Menu size={15} aria-hidden="true" />
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

      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} />}
    </div>
  );
}
