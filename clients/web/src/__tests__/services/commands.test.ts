/**
 * commands.test.ts — the palette registry: availability gates, the
 * generated chat entries mirroring the store, and the shared selectChat
 * path (the sidebar row and the palette must not drift apart).
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { useFlux, resetFluxForTest } from '../../core/state';
import { listCommands, listShortcutRows, selectChat } from '../../services/commands';

function ids(): string[] {
  return listCommands().map((c) => c.id);
}

describe('commands registry', () => {
  beforeEach(() => {
    resetFluxForTest();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('lists the static actions; gated ones respect their gates', () => {
    // resetFluxForTest: disconnected + no chats → reconnect IS offered,
    // search/new-terminal are NOT (no active chat).
    const all = ids();
    expect(all).toContain('action:new-chat');
    expect(all).toContain('action:reconnect');
    expect(all).not.toContain('action:search');
    expect(all).not.toContain('action:new-terminal');
    // Connected: the reconnect action vanishes.
    useFlux.setState({ connectionStatus: 'connected' });
    expect(ids()).not.toContain('action:reconnect');
  });

  it('an active chat unlocks the chat-scoped actions', () => {
    useFlux.setState({ activeChatId: 'c1' });
    // The search action rides the SAME gate as the Ctrl+F intercept: no
    // CSS Highlight API (jsdom) → no entry, native find stays available.
    expect(ids()).not.toContain('action:search');
    expect(ids()).toContain('action:new-terminal');
  });

  it('generates one entry per chat with the workdir as hint/keywords', () => {
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'Parser fix', createdAt: 1, active: false, workdir: '/repo/a', provider: '', model: '' },
        { id: 'c2', name: 'Spike', createdAt: 2, active: false, workdir: '/repo/b', provider: '', model: '' },
      ],
    });
    const chats = listCommands().filter((c) => c.group === 'Chats');
    expect(chats.map((c) => c.id)).toEqual(['chat:c1', 'chat:c2']);
    expect(chats[0].hint).toBe('/repo/a');
    expect(chats[0].keywords).toBe('/repo/a');
  });

  it('actions come before chats (verbs are the repeat visitors)', () => {
    useFlux.setState({
      activeChatId: 'c1',
      chats: [{ id: 'c1', name: 'X', createdAt: 1, active: false, workdir: '', provider: '', model: '' }],
    });
    const listed = listCommands();
    expect(listed[0].group).toBe('Actions');
    expect(listed.at(-1)?.group).toBe('Chats');
  });

  it('exposes a shortcuts-sheet action, and the sheet reads registry chords', () => {
    expect(ids()).toContain('action:shortcuts');
    const rows = listShortcutRows();
    const keys = rows.map((r) => r.keys);
    expect(keys).toContain('Ctrl+K'); // the palette itself — static row
    expect(keys).toContain('Ctrl+B');
    expect(keys).toContain('Ctrl+J');
    expect(keys).toContain('Ctrl+F');
    // Sheet rows are derived FROM the actions' own shortcut+title — every
    // row carries a real label (gates deliberately ignored: the sheet
    // documents bindings, it is not live state).
    for (const r of rows) {
      expect(r.label).toBeTruthy();
    }
  });

  it('selectChat switches the active chat; mobile closes the drawer', () => {
    useFlux.setState({ activeChatId: '', sidebarOpen: true });
    selectChat('c1');
    expect(useFlux.getState().activeChatId).toBe('c1');
    expect(useFlux.getState().sidebarOpen).toBe(true); // desktop: untouched
    const width = Object.getOwnPropertyDescriptor(window, 'innerWidth')?.value;
    Object.defineProperty(window, 'innerWidth', { value: 500, configurable: true, writable: true });
    selectChat('c2');
    expect(useFlux.getState().activeChatId).toBe('c2');
    expect(useFlux.getState().sidebarOpen).toBe(false); // drawer done navigating
    Object.defineProperty(window, 'innerWidth', { value: width, configurable: true, writable: true });
  });
});
