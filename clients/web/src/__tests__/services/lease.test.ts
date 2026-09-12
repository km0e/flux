import { describe, it, expect, beforeEach, vi } from 'vitest';
import { switchLease } from '../../services/lease';
import { useFlux } from '../../core/state';
import { setBridge, resetBridgeForTest } from '../../core/bridge';

describe('switchLease', () => {
  beforeEach(() => {
    resetBridgeForTest();
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
});
