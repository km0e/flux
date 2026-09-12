/**
 * chat-view.test.tsx — the chat column: the interrupt-send (R1 — one fused
 * cancel+send operation), the read-only viewer bar with its Take over
 * button, connection banner.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, fireEvent, screen } from '@testing-library/react';
import { ChatView } from '../../components/ChatView';
import { useFlux, resetFluxForTest } from '../../core/state';
import { resetBridgeForTest, setBridge } from '../../core/bridge';
import { discardInterrupt } from '../../services/stream-handler';
import { _resetPanesForTest } from '../../services/panes';

describe('ChatView', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
    _resetPanesForTest(); // a cached pane from a previous test is detached — panes must rebuild in the live DOM
    document.body.innerHTML = '<div id="messages-wrap"></div>';
  });

  it('send while streaming issues the fused interrupt-send (no separate cancel)', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      connectionStatus: 'connected',
      streaming: { c1: true },
    });
    render(<ChatView />);
    const input = screen.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    input.value = 'new direction';
    fireEvent.keyDown(input, { key: 'Enter' });
    // ONE frame, with the interrupt flag: the server pairs the cancel and
    // the message on one kernel FIFO (R1) — the client sends no separate
    // cancel and parks nothing.
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith({
      type: 'chat',
      chat_id: 'c1',
      message: 'new direction',
      interrupt: true,
    });
    discardInterrupt('c1');
  });

  it('the interrupt-send appends the user bubble immediately (server queues the turn)', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      connectionStatus: 'connected',
      streaming: { c1: true },
    });
    render(<ChatView />);
    const input = screen.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    input.value = 'after cancel';
    fireEvent.keyDown(input, { key: 'Enter' });

    // The bubble is optimistic (the turn is queued server-side, no local
    // park/flush) and streaming stays live (the composer keeps Stop).
    const pane = document.querySelector('.chat-pane') as HTMLElement;
    expect(pane?.querySelector('.message.user')?.textContent).toContain('after cancel');
    expect(useFlux.getState().streaming['c1']).toBe(true);
    discardInterrupt('c1');
  });

  it('read-only chat shows the viewer bar with Take over; clicking re-claims', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      readonlyChats: { c1: true },
    });
    render(<ChatView />);
    expect(screen.getByText(/read-only/i)).toBeTruthy();
    expect(screen.queryByPlaceholderText(/Ask Flux/)).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Take over' }));
    expect(send).toHaveBeenCalledWith({ type: 'chat_claim', chat_id: 'c1' });
    // Optimistic clear: the composer returns.
    expect(useFlux.getState().readonlyChats['c1']).toBeUndefined();
  });

  it('a connection outage shows the banner but KEEPS the composer usable', () => {
    // The composer stays ENABLED through an outage: the message queues in
    // the connection manager and rides out with the reconnect (the banner
    // says exactly that) — blocking the keyboard would throw away the
    // user's thought at the exact moment the connection hiccupped.
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      connectionStatus: 'disconnected',
    });
    render(<ChatView />);
    expect(screen.getByText(/Disconnected — queued messages/)).toBeTruthy();
    const input = screen.getByPlaceholderText(/Ask Flux/) as HTMLTextAreaElement;
    expect(input.disabled).toBe(false);
    // Sending while offline queues the frame (no drop, no interject).
    input.value = 'while offline';
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(send).toHaveBeenCalledWith({ type: 'chat', chat_id: 'c1', message: 'while offline' });
  });

  it('the provider chip sits at the composer footer and opens the switcher', () => {
    const send = vi.fn();
    setBridge({ send });
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: 'p1', model: 'm1' },
      ],
      providers: [{ id: 'p1', url: 'https://p1/v1' }, { id: 'p2', url: 'https://p2/v1' }],
      activeChatId: 'c1',
      connectionStatus: 'connected',
    });
    render(<ChatView />);
    // The chip shows the chat's pin at the input's bottom-left.
    const chip = screen.getByRole('button', { name: 'p1 / m1' });
    fireEvent.click(chip);
    // The switcher dialog opens over the composer.
    expect(screen.getByText('Switch provider')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Switch' }));
    expect(send).toHaveBeenCalledWith({ type: 'chat_provider', chat_id: 'c1', provider: 'p1', model: 'm1' });
  });

  it('the provider chip is inert while offline and hidden from read-only viewers', () => {
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: 'p1', model: 'm1' },
      ],
      activeChatId: 'c1',
      connectionStatus: 'disconnected',
      readonlyChats: {},
    });
    const { rerender } = render(<ChatView />);
    expect((screen.getByRole('button', { name: 'p1 / m1' }) as HTMLButtonElement).disabled).toBe(true);

    // A viewer (another window holds the lease) gets the viewer bar — no
    // composer, no chip: there is no lease to swap with.
    useFlux.setState({ readonlyChats: { c1: true } });
    rerender(<ChatView />);
    expect(screen.queryByRole('button', { name: 'p1 / m1' })).toBeNull();
    expect(screen.getByText(/read-only/i)).toBeTruthy();
  });
});
