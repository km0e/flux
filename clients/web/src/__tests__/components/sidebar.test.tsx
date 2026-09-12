/**
 * sidebar.test.tsx — conversation list interactions: selection = lease
 * handover wiring, inline rename commit, delete confirmation flow, the
 * Radix row menu, client-side filtering.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, screen, waitFor } from '@testing-library/react';
import { Sidebar } from '../../components/Sidebar';
import { useFlux, resetFluxForTest, type Chat } from '../../core/state';
import { resetBridgeForTest, setBridge } from '../../core/bridge';
import { setDialogImpls, resetDialogsForTest } from '../../services/dialogs';

function chat(id: string, over: Partial<Chat> = {}): Chat {
  return { id, name: id, createdAt: Date.now(), kind: 'classic', active: false, workdir: `/tmp/${id}`, provider: '', model: '', ...over };
}

describe('Sidebar', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
    resetDialogsForTest();
    document.body.innerHTML = '<div id="host"></div>';
  });

  it('selecting a chat flips activeChatId (the mount subscription claims)', () => {
    useFlux.setState({ chats: [chat('c1'), chat('c2')], activeChatId: 'c1' });
    render(<Sidebar />);
    fireEvent.click(screen.getByLabelText('Open chat c2'));
    expect(useFlux.getState().activeChatId).toBe('c2');
  });

  it('narrow viewports close the drawer on selection', () => {
    const original = window.innerWidth;
    Object.defineProperty(window, 'innerWidth', { value: 600, configurable: true, writable: true });
    useFlux.setState({ chats: [chat('c1'), chat('c2')], activeChatId: 'c1', sidebarOpen: true });
    render(<Sidebar />);
    fireEvent.click(screen.getByLabelText('Open chat c2'));
    expect(useFlux.getState().sidebarOpen).toBe(false);
    Object.defineProperty(window, 'innerWidth', { value: original, configurable: true, writable: true });
  });

  it('the row menu renames the chat over the wire', async () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({ chats: [chat('c1', { name: 'Old' })], activeChatId: 'c1' });
    render(<Sidebar />);
    fireEvent.pointerDown(screen.getByLabelText('Chat actions for Old'), { button: 0 });
    fireEvent.click(screen.getByLabelText('Chat actions for Old'));
    // Radix renders the menu into a portal on open.
    const item = await screen.findByRole('menuitem', { name: 'Rename' });
    fireEvent.keyDown(item, { key: 'Enter' });
    const input = await screen.findByLabelText('Chat name');
    fireEvent.change(input, { target: { value: 'New name' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() =>
      expect(send).toHaveBeenCalledWith({ type: 'chat_rename', chat_id: 'c1', name: 'New name' }),
    );
  });

  it('the row menu deletes through the confirm dialog', async () => {
    const send = vi.fn();
    setBridge({ send });
    setDialogImpls({ confirmDelete: async () => true });
    useFlux.setState({ chats: [chat('c1', { name: 'Doomed' })], activeChatId: 'c1' });
    render(<Sidebar />);
    fireEvent.pointerDown(screen.getByLabelText('Chat actions for Doomed'), { button: 0 });
    fireEvent.click(screen.getByLabelText('Chat actions for Doomed'));
    fireEvent.keyDown(await screen.findByRole('menuitem', { name: 'Delete' }), { key: 'Enter' });
    await waitFor(() => expect(send).toHaveBeenCalledWith({ type: 'chat_delete', chat_id: 'c1' }));
    expect(useFlux.getState().chats).toHaveLength(0);
  });

  it('a declined confirmation keeps the chat', async () => {
    const send = vi.fn();
    setBridge({ send });
    setDialogImpls({ confirmDelete: async () => false });
    useFlux.setState({ chats: [chat('c1')], activeChatId: 'c1' });
    render(<Sidebar />);
    fireEvent.pointerDown(screen.getByLabelText('Chat actions for c1'), { button: 0 });
    fireEvent.click(screen.getByLabelText('Chat actions for c1'));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Delete' }));
    await waitFor(() => expect(useFlux.getState().chats).toHaveLength(1));
    expect(send).not.toHaveBeenCalled();
  });

  it('the filter narrows the list by name and workdir', () => {
    useFlux.setState({
      chats: [chat('c1', { name: 'Alpha' }), chat('c2', { name: 'Beta', workdir: '/x/gamma' })],
      activeChatId: 'c1',
    });
    render(<Sidebar />);
    const filter = screen.getByLabelText('Filter chats') as HTMLInputElement;
    fireEvent.change(filter, { target: { value: 'gamma' } });
    expect(screen.getByLabelText('Open chat Beta')).toBeTruthy();
    expect(screen.queryByLabelText('Open chat Alpha')).toBeNull();
  });

  it('feature chats carry the kind badge; in-use chats are flagged', () => {
    useFlux.setState({
      chats: [chat('c1', { kind: 'feature' }), chat('c2', { active: true })],
      activeChatId: 'c1',
    });
    render(<Sidebar />);
    expect(screen.getByText('Feature')).toBeTruthy();
    expect(screen.getByText('In use')).toBeTruthy();
  });

  it('the In-use badge is suppressed for rows on an in-flight lease switch', () => {
    // c2's wire `active` flag is stale while the switch lands — rendering
    // it would flash the badge onto the row we just LEFT for a frame.
    useFlux.setState({
      chats: [chat('c1'), chat('c2', { active: true })],
      activeChatId: 'c2',
      leaseSwitch: { from: 'c1', to: 'c2' },
    });
    render(<Sidebar />);
    expect(screen.queryByText('In use')).toBeNull();
  });

  it('relative-time stamps stay fresh on the minute tick (an idle chat never freezes at "now")', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    useFlux.setState({ chats: [chat('c1')], activeChatId: 'c1' }); // createdAt = now → "now"
    render(<Sidebar />);
    expect(screen.getByText('now')).toBeTruthy();
    // Past the minute boundary (two ticks) the stamp re-renders — no chat
    // activity, no chats frame, purely the freshness tick.
    await vi.advanceTimersByTimeAsync(61_000);
    expect(screen.getByText('1m')).toBeTruthy();
    vi.useRealTimers();
  });
});
