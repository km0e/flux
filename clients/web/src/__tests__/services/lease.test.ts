import { describe, it, expect, beforeEach, vi } from 'vitest';
import { switchLease } from '../../services/lease';
import { useFlux } from '../../core/state';
import { setBridge, resetBridgeForTest } from '../../core/bridge';
import { isPaneStale, markPaneStale, _resetStalePanesForTest } from '../../services/stream-handler';
import { getPane, _resetPanesForTest } from '../../services/panes';

describe('switchLease', () => {
  beforeEach(() => {
    resetBridgeForTest();
    _resetStalePanesForTest();
    useFlux.setState({ readonlyChats: {} });
  });

  it('closes the outgoing chat and claims the incoming one', () => {
    const send = vi.fn();
    setBridge({ send });
    switchLease('old', 'new');

    expect(send).toHaveBeenCalledWith({ type: 'chat_close', chat_id: 'old' });
    expect(send).toHaveBeenCalledWith({ type: 'chat_claim', chat_id: 'new' });
  });

  it('optimistically clears read-only on the incoming chat', () => {
    useFlux.getState().setReadOnly('new', true);
    const send = vi.fn();
    setBridge({ send });
    switchLease('', 'new');

    expect(useFlux.getState().readonlyChats['new']).toBeUndefined();
  });

  it('optimistically marks the incoming chat loaded (dedupes the chats-handler re-claim)', () => {
    useFlux.setState({ loadedChatId: '' });
    const send = vi.fn();
    setBridge({ send });
    switchLease('', 'new');

    // loadedChatId's documented purpose is exactly preventing a double claim:
    // the subscription path set it on first connect, so the subsequent chats
    // handler's reopen check (loadedChatId !== active) skips naturally.
    expect(useFlux.getState().loadedChatId).toBe('new');
  });

  it('sends nothing when both ids are empty', () => {
    const send = vi.fn();
    setBridge({ send });
    switchLease('', '');

    expect(send).not.toHaveBeenCalled();
  });

  it('marks the departed pane stale when a round is live at departure', () => {
    useFlux.setState({ streaming: { old: true } });
    const send = vi.fn();
    setBridge({ send });
    switchLease('old', 'new');

    // stream_end is delivered only to subscribers — the pane's DOM and the
    // streaming flag both go stale across the unsubscribe window.
    expect(isPaneStale('old')).toBe(true);
  });

  it('does not mark a quiet departure', () => {
    useFlux.setState({ streaming: {} });
    const send = vi.fn();
    setBridge({ send });
    switchLease('old', 'new');

    expect(isPaneStale('old')).toBe(false);
  });

  it('wipes the incoming stale pane (no outdated flash while the snapshot is in flight)', () => {
    const wrap = document.createElement('div');
    wrap.id = 'messages-wrap';
    document.body.appendChild(wrap);
    const pane = getPane('new');
    const stale = document.createElement('div');
    stale.className = 'message';
    stale.textContent = 'outdated mid-round DOM';
    pane.appendChild(stale);
    markPaneStale('new');

    const send = vi.fn();
    setBridge({ send });
    switchLease('', 'new');

    // The outdated DOM is gone and the empty state shows; the MARK REMAINS —
    // the claim's history render consumes it to override the streaming skip.
    expect(pane.querySelector('.message')).toBeNull();
    expect(isPaneStale('new')).toBe(true);
    document.body.removeChild(wrap);
    _resetPanesForTest();
  });
});
