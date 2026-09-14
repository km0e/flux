/**
 * topbar.test.tsx — the GLOBAL bar: brand, streaming cancel indicator,
 * connection states, the settings menu, theme cycling. Chat identity
 * (name / kind / workdir) and usage live in ChatHeader — the bar above
 * the canvas is app-wide status only.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { TopBar } from '../../components/TopBar';
import { useFlux, resetFluxForTest } from '../../core/state';
import { resetBridgeForTest, setBridge } from '../../core/bridge';

// The settings dialog's fetch-on-show panels hit the management RPCs —
// stubbed here (the dialog wiring is the subject, not the wire).
vi.mock('../../services/providers', () => ({
  fetchProviders: vi.fn(),
  probeProvider: vi.fn(async () => ({ models: [] })),
  handleProviderModelsReply: vi.fn(),
}));
vi.mock('../../services/models', () => ({ fetchModels: vi.fn(), saveModel: vi.fn(async () => ({})) }));
vi.mock('../../services/mcp', () => ({ fetchMcpServers: vi.fn(), addMcpServer: vi.fn(async () => undefined), removeMcpServer: vi.fn(async () => undefined) }));
vi.mock('../../services/skills', () => ({ fetchSkills: vi.fn(), addSkill: vi.fn(async () => undefined), removeSkill: vi.fn(async () => undefined) }));

describe('TopBar', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
    document.body.innerHTML = '<div id="host"></div>';
    delete document.documentElement.dataset.theme;
  });

  it('shows the app brand (logo + name)', () => {
    const { container } = render(<TopBar />);
    expect(screen.getByText('Flux')).toBeTruthy();
    // The VSCode-era logo rides /assets like any other static asset.
    expect(container.querySelector('img[src="/assets/favicon.svg"]')).toBeTruthy();
  });

  it('carries NO chat identity — that moved to ChatHeader', () => {
    useFlux.setState({
      chats: [
        {
          id: 'c1',
          name: 'Refactor',
          createdAt: 1,
          active: false,
          workdir: '/tmp/proj',
          provider: 'default',
          model: 'gpt-4o-mini',
        },
      ],
      activeChatId: 'c1',
    });
    render(<TopBar />);
    expect(screen.queryByText('Refactor')).toBeNull();
    expect(screen.queryByText('/tmp/proj')).toBeNull();
  });

  it('the settings gear opens the settings dialog DIRECTLY (no menu)', async () => {
    render(<TopBar />);
    // No dropdown indirection: the gear itself opens the one settings
    // dialog, landing on the last-visited section (the module default is
    // Providers; this file never switches tabs, so it stays deterministic).
    fireEvent.click(screen.getByRole('button', { name: 'Settings' }));
    const dlg = await screen.findByRole('dialog');
    expect(dlg).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'Providers', selected: true })).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'MCP servers' })).toBeTruthy();
    expect(screen.getByRole('tab', { name: 'Skills' })).toBeTruthy();
    // Closing returns to a clean bar (reopening starts fresh from the
    // dialog's own last-tab memory).
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('the streaming indicator cancels the round on click', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      streaming: { c1: true },
    });
    render(<TopBar />);
    fireEvent.click(screen.getByRole('button', { name: /working/i }));
    expect(send).toHaveBeenCalledWith({ type: 'cancel', chat_id: 'c1' });
  });

  it('a failed connection offers reconnect', () => {
    const reconnect = vi.fn();
    setBridge({ reconnect });
    useFlux.setState({ connectionStatus: 'failed' });
    render(<TopBar />);
    fireEvent.click(screen.getByRole('button', { name: /reconnect/i }));
    expect(reconnect).toHaveBeenCalledTimes(1);
  });

  it('theme toggle cycles auto → dark → light → auto and pins data-theme', () => {
    render(<TopBar />);
    const btn = screen.getByRole('button', { name: /theme/i });
    fireEvent.click(btn);
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(localStorage.getItem('flux.theme')).toBe('dark');
    fireEvent.click(btn);
    expect(document.documentElement.dataset.theme).toBe('light');
    fireEvent.click(btn);
    // auto removes the override — the media query owns it again
    expect(document.documentElement.dataset.theme).toBeUndefined();
    expect(localStorage.getItem('flux.theme')).toBeNull();
  });
});

describe('TopBar — the MCP notice bell (F-10b)', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
    document.body.innerHTML = '<div id="host"></div>';
  });

  it('shows the unread badge and opens the ring; opening marks it read', async () => {
    useFlux.setState({
      mcpNotices: [
        { id: 2, server_id: 'fs', level: 'info', message: 'indexing done', at: Date.now() },
        { id: 1, server_id: 'fs', level: 'warning', message: 'slow upstream', at: Date.now() },
      ],
      mcpNoticesUnread: 2,
    });
    render(<TopBar />);
    // The badge counts what arrived since the bell was last open.
    expect(screen.getByText('2')).toBeTruthy();
    const bell = screen.getByLabelText(/Notifications \(2 unread\)/);
    // Radix menus open on pointerdown (the repo's established pattern).
    fireEvent.pointerDown(bell, { button: 0 });
    fireEvent.click(bell);
    // The ring lists newest first, source-labelled + levelled (Radix
    // mounts the menu content in a portal — find async).
    expect(await screen.findByText('indexing done')).toBeTruthy();
    expect(screen.getByText('slow upstream')).toBeTruthy();
    expect(screen.getByText('warning')).toBeTruthy();
    expect(useFlux.getState().mcpNoticesUnread).toBe(0);
  });

  it('an empty ring reads as an honest empty state', async () => {
    render(<TopBar />);
    expect(screen.queryByText('2')).toBeNull();
    const bell = screen.getByLabelText(/Notifications/);
    fireEvent.pointerDown(bell, { button: 0 });
    fireEvent.click(bell);
    expect(await screen.findByText(/Nothing yet/)).toBeTruthy();
  });
});
