/**
 * state.test.ts — the global store (zustand) behavior.
 *
 * Covers the derived logic that used to live on AppState: chat list
 * convergence, per-chat pruning, streaming flags, readonly marks. The
 * store is a module singleton — resetFluxForTest() isolates each test.
 *
 * Provides: store behavior tests
 * Depends: ../core/state
 */
import { describe, it, expect, beforeEach } from 'vitest';
import { useFlux, resetFluxForTest, type Chat } from '../../core/state';

function chat(id: string, over: Partial<Chat> = {}): Chat {
  return {
    id,
    name: id,
    createdAt: 1,
    active: false,
    workdir: '/tmp/proj',
    provider: '',
    model: '',
    ...over,
  };
}

describe('store — chats', () => {
  beforeEach(() => resetFluxForTest());

  it('starts with an empty chat list and activeChatId', () => {
    expect(useFlux.getState().chats).toEqual([]);
    expect(useFlux.getState().activeChatId).toBe('');
  });

  it('setChats replaces the list and auto-selects the first chat', () => {
    useFlux.getState().setChats([chat('c1'), chat('c2')]);
    expect(useFlux.getState().chats).toHaveLength(2);
    expect(useFlux.getState().activeChatId).toBe('c1');
  });

  it('setChats sorts newest first (the wire order is HashMap-random)', () => {
    useFlux.getState().setChats([
      chat('old', { createdAt: 100 }),
      chat('new', { createdAt: 300 }),
      chat('mid', { createdAt: 200 }),
    ]);
    expect(useFlux.getState().chats.map((c) => c.id)).toEqual(['new', 'mid', 'old']);
  });

  it('setChats ends the lease-switch suppression only on a frame that carries the release', () => {
    // A frame where the from-side is STILL leased (e.g. the fork's attach
    // broadcast, sent before the release) must not end the window — the
    // left row would flash In-use on stale truth.
    useFlux.setState({ leaseSwitch: { from: 'c1', to: 'c2' } });
    useFlux
        .getState()
        .setChats([chat('c1', { active: true }), chat('c2', { active: true })]);
    expect(useFlux.getState().leaseSwitch).toEqual({ from: 'c1', to: 'c2' });
    // The release confirmation: the from-side freed → the window ends.
    useFlux.getState().setChats([chat('c1'), chat('c2', { active: true })]);
    expect(useFlux.getState().leaseSwitch).toBeNull();
  });

  it('setChats treats a vanished from-row as release confirmation', () => {
    useFlux.setState({ leaseSwitch: { from: 'c1', to: 'c2' } });
    useFlux.getState().setChats([chat('c2')]);
    expect(useFlux.getState().leaseSwitch).toBeNull();
  });

  it('setChats re-selects when the active chat was deleted elsewhere', () => {
    useFlux.getState().setChats([chat('c1'), chat('c2')]);
    useFlux.setState({ activeChatId: 'c2' });
    useFlux.getState().setChats([chat('c1')]);
    expect(useFlux.getState().activeChatId).toBe('c1');
  });

  it('setChats clears activeChatId when all chats are gone', () => {
    useFlux.getState().setChats([chat('c1')]);
    useFlux.getState().setChats([]);
    expect(useFlux.getState().activeChatId).toBe('');
  });

  it('addChat prepends, deduplicates by id, and selects the chat', () => {
    useFlux.getState().addChat(chat('a'));
    useFlux.getState().addChat(chat('b'));
    useFlux.getState().addChat(chat('a', { name: 'a2' }));
    const chats = useFlux.getState().chats;
    expect(chats.map((c) => c.id)).toEqual(['a', 'b']);
    expect(chats[0].name).toBe('a2');
    expect(useFlux.getState().activeChatId).toBe('a');
  });

  it('renameChat updates the name immutably', () => {
    useFlux.getState().setChats([chat('c1')]);
    const before = useFlux.getState().chats[0];
    useFlux.getState().renameChat('c1', 'renamed');
    const chats = useFlux.getState().chats;
    expect(chats[0].name).toBe('renamed');
    expect(chats[0]).not.toBe(before); // new object, no mutation
  });

  it('deleteChat removes the chat and switches to a remaining one', () => {
    useFlux.getState().setChats([chat('c1'), chat('c2')]);
    useFlux.setState({ activeChatId: 'c2' });
    useFlux.getState().deleteChat('c2');
    expect(useFlux.getState().chats.map((c) => c.id)).toEqual(['c1']);
    expect(useFlux.getState().activeChatId).toBe('c1');
  });

  it('deleteChat prunes every per-chat record', () => {
    useFlux.getState().setChats([chat('c1')]);
    useFlux.getState().setStreaming('c1', true);
    useFlux.getState().setReadOnly('c1', true);
    useFlux.getState().addUsage('c1', {
      prompt_tokens: 10,
      completion_tokens: 5,
      cached_tokens: 0,
    });
    useFlux.getState().deleteChat('c1');
    const s = useFlux.getState();
    expect(s.streaming['c1']).toBeUndefined();
    expect(s.readonlyChats['c1']).toBeUndefined();
    expect(s.usage['c1']).toBeUndefined();
    expect(s.loadedChatId).toBe('');
  });
});

describe('store — readonly marks', () => {
  beforeEach(() => resetFluxForTest());

  it('setReadOnly marks only the target chat', () => {
    useFlux.getState().setReadOnly('c1', true);
    useFlux.getState().setReadOnly('c2', true);
    useFlux.getState().setReadOnly('c1', false);
    expect(useFlux.getState().readonlyChats).toEqual({ c2: true });
  });
});

describe('store — streaming flags', () => {
  beforeEach(() => {
    resetFluxForTest();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
  });

  it('setStreaming tracks per-chat streaming', () => {
    useFlux.getState().setStreaming('c1', true);
    expect(useFlux.getState().streaming['c1']).toBe(true);
    useFlux.getState().clearStreaming('c1');
    expect(useFlux.getState().streaming['c1']).toBeUndefined();
  });

  // (the scroll-button recomputation moved out of the store — a store
  // action must not reach into the DOM; pinned in message-list.test.tsx)
});

describe('store — usage accumulation', () => {
  beforeEach(() => resetFluxForTest());

  it('addUsage folds rounds into compact totals (W = in − R)', () => {
    useFlux.getState().addUsage('c1', {
      prompt_tokens: 100,
      completion_tokens: 50,
      cached_tokens: 25,
    });
    useFlux.getState().addUsage('c1', {
      prompt_tokens: 200,
      completion_tokens: 80,
      cached_tokens: 150,
    });
    expect(useFlux.getState().usage['c1']).toEqual({
      inTokens: 300,
      outTokens: 130,
      cachedTokens: 175,
      contextTokens: 200,
    });
  });
});

describe('store — dock tabs', () => {
  beforeEach(() => resetFluxForTest());

  it('addTerminalTab opens the dock (the explicit user ask)', () => {
    useFlux.getState().addTerminalTab('c1');
    expect(useFlux.getState().dockOpen).toBe(true);
    expect(useFlux.getState().activeDockTab).toBe(useFlux.getState().terminalTabs[0].id);
  });

  it('addTerminalTab(chatId, false) restores silently — a closed dock stays closed', () => {
    // The refresh-restore path: tabs come back, the user's closed dock
    // must not pop open over them.
    useFlux.setState({ dockOpen: false });
    useFlux.getState().addTerminalTab('c1', false);
    expect(useFlux.getState().dockOpen).toBe(false);
    expect(useFlux.getState().terminalTabs).toHaveLength(1);
  });
});
