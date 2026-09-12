/**
 * chat-header.test.tsx — the conversation's header row: identity (name,
 * workdir) + per-chat usage. The global TopBar carries none
 * of these (it is app-wide status only) — the identity moved here.
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { ChatHeader } from '../../components/ChatHeader';
import { useFlux, resetFluxForTest } from '../../core/state';

describe('ChatHeader', () => {
  beforeEach(() => {
    resetFluxForTest();
  });

  it('renders nothing without an active chat', () => {
    const { container } = render(<ChatHeader />);
    expect(container.querySelector('#chat-header')).toBeNull();
  });

  it('carries the chat identity: name, workdir', () => {
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
    render(<ChatHeader />);
    expect(screen.getByText('Refactor')).toBeTruthy();
    expect(screen.getByText('/tmp/proj')).toBeTruthy();
  });

  it('shows the per-chat usage when the chat has consumed tokens', () => {
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      usage: { c1: { inTokens: 120, outTokens: 40, cachedTokens: 8, contextTokens: 120 } },
    });
    render(<ChatHeader />);
    expect(document.getElementById('usage-stats')).toBeTruthy();
  });

  it("the dock toggle flips dockOpen and tracks it via aria-pressed", () => {
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'R', createdAt: 1, active: false, workdir: '', provider: '', model: '' },
      ],
      activeChatId: 'c1',
      dockOpen: false,
    });
    const { rerender } = render(<ChatHeader />);
    const toggle = () => screen.getByRole('button', { name: 'Toggle dock' });
    expect(toggle().getAttribute('aria-pressed')).toBe('false');
    // Direct dock affordance — opening must not require opening a file or
    // spawning a terminal first.
    fireEvent.click(toggle());
    expect(useFlux.getState().dockOpen).toBe(true);
    rerender(<ChatHeader />);
    expect(toggle().getAttribute('aria-pressed')).toBe('true');
    fireEvent.click(toggle());
    expect(useFlux.getState().dockOpen).toBe(false);
  });
});
