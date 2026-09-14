import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { registerAllHandlers } from '../../services/handlers';
import { dispatchMessage } from '../../services/dispatch';
import { useFlux } from '../../core/state';
import { ensurePane, getPaneIfExists, _resetPanesForTest } from '../../services/panes';
import { getController } from '../../services/stream';
import { markInterrupt } from '../../services/stream-handler';
import { bridge, setBridge, resetBridgeForTest } from '../../core/bridge';
import { stashForkDraft, takePendingDraft } from '../../services/forkDraft';
import { setDialogImpls, resetDialogsForTest } from '../../services/dialogs';
import type { ConnectionLike } from '../../services/dispatch';
import type { ServerMessage } from '../../core/types';

/** A controllable dialogs mock: askQuestion waits for the test to resolve
 * the deferred answer (the impl owns the UX; the service only calls it). */
function makeDialogsMock() {
  let resolveAsk: ((answer: string) => void) | null = null;
  const impls = {
    confirmDelete: async () => false,
    pickNewChat: async () => null,
    askQuestion: () =>
      new Promise<string>((resolve) => {
        resolveAsk = resolve;
      }),
  };
  return {
    impls,
    answer: (a: string) => resolveAsk?.(a),
  };
}

function mockCtx() {
  const conn = {
    send: vi.fn(),
    updateConfig: vi.fn(),
    reconnect: vi.fn(),
  } as unknown as ConnectionLike;
  return {
    ctx: {
      get state() {
        return useFlux.getState();
      },
      conn,
      bridge,
    },
    conn,
  };
}

describe('handlers', () => {
  let dialogsMock: ReturnType<typeof makeDialogsMock>;

  beforeEach(() => {
    registerAllHandlers();
    resetBridgeForTest();
    resetDialogsForTest();
    dialogsMock = makeDialogsMock();
    setDialogImpls(dialogsMock.impls);
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
    useFlux.setState({ chats: [] });
    useFlux.setState({ activeChatId: '' });
    useFlux.setState({ loadedChatId: '' });
    useFlux.setState({ streaming: {} });
    // reasoning blocks live in the stream module's registry now — nothing
    // to reset here (handlers.ts never touches them directly).

    useFlux.setState({ usage: {} });
    useFlux.setState({ readonlyChats: {} });
    sessionStorage.clear();
  });

  function chatsMsg(): ServerMessage {
    return {
      type: 'chats',
      chats: [
        {
          chat_id: 'c1',
          name: 'One',
          created_at: '2026-01-01T00:00:00Z', last_activity_at: '2026-01-01T00:00:00Z',
          workdir: '/tmp/proj',
        provider: 'default',
        model: 'gpt-4o-mini',
          active: false,
        },
      ],
    };
  }

  it('prunes panes of chats missing from the authoritative list', () => {
    const { ctx } = mockCtx();
    // Panes (one with a live controller) for chats that no longer exist
    // server-side — deleted outside the confirm flow (e.g. another panel).
    getController('ghost');
    useFlux.getState().setStreaming('ghost', true);
    ensurePane('ghost');
    ensurePane('c2');
    ensurePane('c1');

    dispatchMessage(chatsMsg(), ctx); // the list contains only c1

    expect(getPaneIfExists('ghost')).toBeUndefined();
    expect(useFlux.getState().streaming['ghost']).toBeUndefined();
    expect(getPaneIfExists('c2')).toBeUndefined();
    expect(getPaneIfExists('c1')).toBeDefined(); // surviving chat untouched
  });

  it('sends chat_claim when setChats auto-selects the first chat', () => {
    const { ctx, conn } = mockCtx();
    dispatchMessage(chatsMsg(), ctx);

    expect(useFlux.getState().activeChatId).toBe('c1');
    expect(conn.send).toHaveBeenCalledWith({ type: 'chat_claim', chat_id: 'c1' });
  });

  it('does not resend chat_claim on a refresh for the already-loaded chat', () => {
    useFlux.setState({ activeChatId: 'c1' });
    useFlux.setState({ loadedChatId: 'c1' });
    const { ctx, conn } = mockCtx();
    dispatchMessage(chatsMsg(), ctx);

    expect(conn.send).not.toHaveBeenCalledWith({ type: 'chat_claim', chat_id: 'c1' });
  });

  it('marks the chat loaded when its history arrives', () => {
    const { ctx } = mockCtx();
    dispatchMessage({ type: 'chat_history', chat_id: 'c1', messages: [] }, ctx);

    expect(useFlux.getState().loadedChatId).toBe('c1');
  });

  it("a fork's chat_created pairs the stashed redo-turn draft with the new chat", () => {
    stashForkDraft('c1', 'redo me');
    const { ctx } = mockCtx();
    dispatchMessage(
      {
        type: 'chat_created',
        chat: {
          chat_id: 'fork-1',
          name: 'One (fork)',
          created_at: '2026-01-01T00:00:00Z', last_activity_at: '2026-01-01T00:00:00Z',
          workdir: '/tmp/proj',
          provider: 'default',
          model: 'gpt-4o-mini',
          active: false,
          forked_from_chat_id: 'c1',
        },
      },
      ctx,
    );
    // addChat auto-selected the fork; the draft now belongs to it and its
    // composer consumes it on mount (once).
    expect(useFlux.getState().activeChatId).toBe('fork-1');
    expect(takePendingDraft('fork-1')).toBe('redo me');
    expect(takePendingDraft('fork-1')).toBeUndefined();
  });

  it('question_required: host answers, wait bar dismissed, question_response sent', async () => {
    const { ctx, conn } = mockCtx();
    const pane = ensurePane('c1');

    dispatchMessage(
      {
        type: 'question_required',
        chat_id: 'c1',
        id: 'q1',
        question: { text: 'Which database?', options: ['postgres', 'sqlite'] },
      },
      ctx,
    );
    expect(pane.querySelector('#question-wait')?.textContent).toContain('Waiting for your answer');

    // The host's answer arrives (resolve askQuestion) → question_response is sent.
    dialogsMock.answer('postgres');
    await vi.waitFor(() => {
      expect(conn.send).toHaveBeenCalledWith({
        type: 'question_response',
        chat_id: 'c1',
        id: 'q1',
        answer: 'postgres',
      });
    });
    expect(pane.querySelector('#question-wait')).toBeNull();
  });

  it('does not re-fetch the chat list when a round ends (message counts are gone)', () => {
    const { ctx, conn } = mockCtx();
    dispatchMessage({ type: 'stream_end', chat_id: 'c1' }, ctx);

    expect(conn.send).not.toHaveBeenCalled();
  });

  it('shows a neutral notice (not an error) when the round is cancelled', () => {
    const { ctx } = mockCtx();
    useFlux.getState().setStreaming('c1', true);
    dispatchMessage({ type: 'stream_cancelled', chat_id: 'c1' }, ctx);

    expect(useFlux.getState().streaming['c1']).toBeUndefined();
    const pane = document.querySelector('.chat-pane') as HTMLElement;
    const noticeEl = pane?.querySelector('.message.notice') as HTMLElement;
    expect(noticeEl).toBeDefined();
    if (noticeEl) {
      expect(noticeEl.textContent).toContain('Cancelled');
    }
    expect(pane?.querySelector('.message.error')).toBeNull();
  });

  it('chat_busy marks the chat read-only and demotes to viewer via chat_open', () => {
    // The read-only viewer bar (with its Take over button) is ChatView UI,
    // driven reactively by the readonlyChats flag — pinned in chat-view.test.
    // The service contract is the flag + the viewer demotion wire message.
    const { ctx, conn } = mockCtx();
    ensurePane('c1'); // claim/send both originate from an already-open pane
    dispatchMessage({ type: 'error', chat_id: 'c1', code: 'chat_busy', message: 'busy' }, ctx);

    expect(useFlux.getState().readonlyChats['c1']).toBe(true);
    // D-06: a failed claim degrades to viewer — a follow-up chat_open
    // establishes the subscription and pulls history (the server skips the
    // history resend idempotently when already subscribed).
    expect(conn.send).toHaveBeenCalledWith({ type: 'chat_open', chat_id: 'c1' });
  });

  it('a freed lease (active:false in the chats list) clears a stale read-only mark', () => {
    const { ctx } = mockCtx();
    useFlux.getState().setReadOnly('c1', true);
    dispatchMessage(
      {
        type: 'chats',
        chats: [
          { chat_id: 'c1', name: 'One', created_at: '2026-01-01T00:00:00Z', last_activity_at: '2026-01-01T00:00:00Z', workdir: '/tmp', active: false, provider: 'default', model: 'gpt-4o-mini' },
        ],
      },
      ctx,
    );
    expect(useFlux.getState().readonlyChats['c1']).toBeUndefined();
  });

  it('stream_gap re-subscribes via chat_claim for the lease holder (open would demote)', () => {
    const { ctx, conn } = mockCtx();
    // handleStreamGap re-opens via the bridge — wire it to the conn mock so
    // the click is observable on conn.send.
    setBridge({ send: conn.send });
    dispatchMessage({ type: 'error', chat_id: 'c1', code: 'stream_gap', message: 'gap' }, ctx);

    const pane = ensurePane('c1');
    const gapEl = pane?.querySelector('.stream-gap') as HTMLElement;
    expect(gapEl).toBeDefined();
    if (gapEl) {
      gapEl.click();
      // Not read-only = we hold the lease: after close clears the dropped-frame
      // mark (ViewerGone), the reopen MUST use claim — open would silently
      // downgrade the lease holder to a viewer.
      expect(conn.send).toHaveBeenCalledWith({ type: 'chat_close', chat_id: 'c1' });
      expect(conn.send).toHaveBeenCalledWith({ type: 'chat_claim', chat_id: 'c1' });
    }
  });

  it('stream_gap falls back to chat_open for a read-only viewer', () => {
    const { ctx, conn } = mockCtx();
    setBridge({ send: conn.send });
    useFlux.getState().setReadOnly('c1', true);
    dispatchMessage({ type: 'error', chat_id: 'c1', code: 'stream_gap', message: 'gap' }, ctx);

    const pane = ensurePane('c1');
    const gapEl = pane?.querySelector('.stream-gap') as HTMLElement;
    if (gapEl) {
      gapEl.click();
      // A pure viewer has no lease to recover: keep the open path.
      expect(conn.send).toHaveBeenCalledWith({ type: 'chat_open', chat_id: 'c1' });
    }
  });

  it('other error codes surface as stream errors in the chat pane', () => {
    const { ctx } = mockCtx();
    useFlux.getState().setStreaming('c1', true);
    dispatchMessage({ type: 'error', chat_id: 'c1', code: 'stream_crashed', message: 'boom' }, ctx);

    expect(useFlux.getState().streaming['c1']).toBeUndefined();
    const pane = ensurePane('c1');
    const errEl = pane?.querySelector('.message.error') as HTMLElement;
    expect(errEl).toBeDefined();
    if (errEl) expect(errEl.textContent).toContain('boom');
  });

  it('error without chat_id surfaces as a server-level error bubble', () => {
    const { ctx } = mockCtx();
    dispatchMessage({ type: 'error', code: 'internal', message: 'boom' }, ctx);

    const wrap = document.getElementById('messages-wrap');
    expect(wrap?.querySelector('.message.error')).toBeTruthy();
  });

  it('chat_state idle clears streaming and retires interrupt bookkeeping (snapshot is authoritative)', () => {
    const { ctx } = mockCtx();
    useFlux.getState().setStreaming('c1', true);
    useFlux.setState({ activeChatId: 'c1' });
    useFlux.setState({ loadedChatId: 'c1' });
    // An interrupt-send whose wrap-up events were lost (gap): the flag is
    // live until the snapshot retires it.
    markInterrupt('c1');

    dispatchMessage({ type: 'chat_state', chat_id: 'c1', state: 'idle' }, ctx);

    // The snapshot confirms the round ended → streaming clears; the retired
    // flag cannot keep a later round's stream_end from clearing.
    expect(useFlux.getState().streaming['c1']).toBeUndefined();
    useFlux.getState().setStreaming('c1', true);
    dispatchMessage({ type: 'stream_end', chat_id: 'c1' }, ctx);
    expect(useFlux.getState().streaming['c1']).toBeUndefined();
  });

  it('chat_state streaming sets the streaming flag (round in flight)', () => {
    const { ctx } = mockCtx();
    dispatchMessage({ type: 'chat_state', chat_id: 'c1', state: 'streaming' }, ctx);

    expect(useFlux.getState().streaming['c1']).toBe(true);
  });

  it('session_resumed stores a fresh identity and pulls the chat list (first visit)', () => {
    const { ctx, conn } = mockCtx();
    // The connect plane's ready element synthesizes session_resumed for
    // fresh AND adopted identities — one handler covers both.
    dispatchMessage({ type: 'session_resumed', session_id: 's-1', leases: [] }, ctx);
    expect(sessionStorage.getItem('flux.session.id')).toBe('s-1');
    expect(conn.send).toHaveBeenCalledWith({ type: 'chat_list' });
  });

  it('session_resumed stores the authoritative id and restores a leased focus', () => {
    const { ctx, conn } = mockCtx();
    dispatchMessage(
      {
        type: 'session_resumed',
        session_id: 'adopted-1',
        leases: ['c9', 'c7'],
      },
      ctx,
    );
    expect(sessionStorage.getItem('flux.session.id')).toBe('adopted-1');
    // No active chat yet → the first leased chat becomes the focus.
    expect(useFlux.getState().activeChatId).toBe('c9');
    expect(conn.send).toHaveBeenCalledWith({ type: 'chat_list' });
  });

  it('session_resumed keeps the current focus when one exists', () => {
    useFlux.setState({ activeChatId: 'mine' });
    const { ctx, conn } = mockCtx();
    dispatchMessage({ type: 'session_resumed', session_id: 'adopted-2', leases: ['c9'] }, ctx);
    expect(useFlux.getState().activeChatId).toBe('mine');
    expect(conn.send).toHaveBeenCalledWith({ type: 'chat_list' });
  });

  it('provider_switched updates the chat and drops a pane notice', () => {
    const { ctx } = mockCtx();
    useFlux.setState({
      chats: [
        { id: 'c1', name: 'One', createdAt: 1, active: true, workdir: '/tmp', provider: 'default', model: 'm1' },
      ],
      activeChatId: 'c1',
    });
    const pane = ensurePane('c1');
    dispatchMessage(
      { type: 'provider_switched', chat_id: 'c1', provider: 'swap', model: 'custom-model' },
      ctx,
    );
    const chat = useFlux.getState().chats.find((c) => c.id === 'c1');
    expect(chat?.provider).toBe('swap');
    expect(chat?.model).toBe('custom-model');
    expect(pane.textContent).toContain('swap · custom-model');
  });

  it('providers reply fills the registry cache', () => {
    const { ctx } = mockCtx();
    dispatchMessage(
      { type: 'providers', providers: [{ id: 'a', url: 'https://a/v1' }, { id: 'b', url: 'https://b/v1' }] },
      ctx,
    );
    expect(useFlux.getState().providers).toEqual([
      { id: 'a', url: 'https://a/v1' },
      { id: 'b', url: 'https://b/v1' },
    ]);
  });
});

describe('tool_preview lifecycle (pending card → upgrade / void)', () => {
  beforeEach(() => {
    registerAllHandlers();
    resetBridgeForTest();
    resetDialogsForTest();
    setDialogImpls(makeDialogsMock().impls);
    document.body.innerHTML = '<div id="messages-wrap"></div>';
    _resetPanesForTest();
    useFlux.setState({ chats: [] });
    useFlux.setState({ activeChatId: '' });
    useFlux.setState({ loadedChatId: '' });
    useFlux.setState({ streaming: {} });
    useFlux.setState({ usage: {} });
    useFlux.setState({ readonlyChats: {} });
    sessionStorage.clear();
  });

  afterEach(() => {
    resetDialogsForTest();
    resetBridgeForTest();
  });

  it('tool_preview creates a pending card; tool_start upgrades it in place', () => {
    const { ctx } = mockCtx();
    dispatchMessage({ type: 'tool_preview', chat_id: 'c1', id: 'p1', name: 'bash' }, ctx);
    let card = document.querySelector<HTMLElement>('[data-tool-call-id="p1"]')!;
    expect(card.classList.contains('pending')).toBe(true);

    dispatchMessage(
      { type: 'tool_preview', chat_id: 'c1', id: 'p1', arguments_delta: '{"cmd":"x"}' },
      ctx,
    );
    expect(card.dataset.rawArgs).toBe('{"cmd":"x"}');

    dispatchMessage(
      { type: 'tool_start', chat_id: 'c1', id: 'p1', name: 'bash', arguments: '{"cmd":"x"}' },
      ctx,
    );
    card = document.querySelector<HTMLElement>('[data-tool-call-id="p1"]')!;
    expect(card.classList.contains('pending')).toBe(false);
    expect(document.querySelectorAll('[data-tool-call-id="p1"]').length).toBe(1);
  });

  it('stream_end / stream_cancelled / stream_error void never-upgraded previews', () => {
    const { ctx } = mockCtx();
    const make = () => {
      dispatchMessage({ type: 'tool_preview', chat_id: 'c1', id: 'p2', name: 'grep' }, ctx);
      return document.querySelector('[data-tool-call-id="p2"]')!;
    };

    // stream_end voids.
    make();
    dispatchMessage({ type: 'stream_end', chat_id: 'c1' }, ctx);
    expect(document.querySelectorAll('[data-tool-call-id="p2"]').length).toBe(0);

    // stream_cancelled voids.
    make();
    dispatchMessage({ type: 'stream_cancelled', chat_id: 'c1' }, ctx);
    expect(document.querySelectorAll('[data-tool-call-id="p2"]').length).toBe(0);

    // stream_error voids.
    make();
    dispatchMessage({ type: 'error', chat_id: 'c1', code: 'internal', message: 'boom' }, ctx);
    expect(document.querySelectorAll('[data-tool-call-id="p2"]').length).toBe(0);
  });

  it('mcp_notice: warning+ toasts; everything lands in the bell ring (unread counted)', () => {
    const { ctx } = mockCtx();
    dispatchMessage({ type: 'mcp_notice', server_id: 'fs', level: 'warning', message: 'slow upstream' }, ctx);
    dispatchMessage({ type: 'mcp_notice', server_id: 'fs', level: 'info', message: 'tick' }, ctx);
    const s = useFlux.getState();
    // The ring keeps both, newest first, source-labelled.
    expect(s.mcpNotices.map((n) => n.message)).toEqual(['tick', 'slow upstream']);
    expect(s.mcpNoticesUnread).toBe(2);
    // warning+ pops a toast; info is ring-only.
    expect(s.toasts.some((t) => t.text.includes('[mcp:fs] slow upstream'))).toBe(true);
    expect(s.toasts.some((t) => t.text.includes('tick'))).toBe(false);
    // Opening the bell clears the unread counter, keeps the ring.
    s.markMcpNoticesRead();
    expect(useFlux.getState().mcpNoticesUnread).toBe(0);
    expect(useFlux.getState().mcpNotices).toHaveLength(2);
  });

  it('a preview upgraded to running survives round end (only pending cards void)', () => {
    const { ctx } = mockCtx();
    dispatchMessage({ type: 'tool_preview', chat_id: 'c1', id: 'p3', name: 'bash' }, ctx);
    dispatchMessage(
      { type: 'tool_start', chat_id: 'c1', id: 'p3', name: 'bash', arguments: '{}' },
      ctx,
    );
    dispatchMessage({ type: 'stream_end', chat_id: 'c1' }, ctx);
    expect(document.querySelectorAll('[data-tool-call-id="p3"]').length).toBe(1);
  });
});
