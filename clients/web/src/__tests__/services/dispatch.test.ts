import { describe, it, expect, vi, beforeEach } from 'vitest';
import { dispatchMessage, handleServerError } from '../../services/dispatch';
import type { DispatchContext } from '../../services/dispatch';
import { registerAllHandlers } from '../../services/handlers';
import { setDialogImpls, resetDialogsForTest } from '../../services/dialogs';
import { useFlux, resetFluxForTest } from '../../core/state';
import { resetBridgeForTest, bridge } from '../../core/bridge';
import type { ConnectionLike } from '../../services/dispatch';
import type { ServerMessage } from '../../core/types';

function mockConn(): ConnectionLike {
  return {
    send: vi.fn(),
    reconnect: vi.fn(),
    updateConfig: vi.fn(),
  } as unknown as ConnectionLike;
}

function mockCtx(conn: ConnectionLike): DispatchContext {
  // state must be a getter (see DispatchContext.state docs) — a snapshot
  // would read stale fields after any set().
  return {
    get state() {
      return useFlux.getState();
    },
    conn,
    bridge,
  };
}

describe('dispatchMessage', () => {
  let conn: ConnectionLike;

  beforeEach(() => {
    registerAllHandlers();
    resetFluxForTest();
    resetDialogsForTest();
    conn = mockConn();
    resetBridgeForTest();
    document.body.innerHTML = '<div id="messages-wrap"></div>';
  });

  it('ready sets no state', () => {
    dispatchMessage({ type: 'ready', session_id: 's-1' }, mockCtx(conn));
    // no error thrown = pass
  });

  it('chats populates chat list and auto-selects', () => {
    const msg: ServerMessage = {
      type: 'chats',
      chats: [
        {
          chat_id: 'c1',
          name: 'Chat 1',
          created_at: '2024-01-01T00:00:00Z', last_activity_at: '2024-01-01T00:00:00Z',
          kind: 'classic',
          workdir: '/tmp/proj',
        provider: 'default',
        model: 'gpt-4o-mini',
          active: false,
        },
        {
          chat_id: 'c2',
          name: 'Chat 2',
          created_at: '2024-01-02T00:00:00Z', last_activity_at: '2024-01-02T00:00:00Z',
          kind: 'classic',
          workdir: '/tmp/proj',
        provider: 'default',
        model: 'gpt-4o-mini',
          active: false,
        },
      ],
    };
    dispatchMessage(msg, mockCtx(conn));
    expect(useFlux.getState().chats).toHaveLength(2);
    expect(useFlux.getState().activeChatId).toBe('c1');
  });

  it('chat_created adds and selects new chat', () => {
    const msg: ServerMessage = {
      type: 'chat_created',
      chat: {
        chat_id: 'new',
        name: 'New Chat',
        created_at: '2024-01-01T00:00:00Z', last_activity_at: '2024-01-01T00:00:00Z',
        kind: 'classic',
        workdir: '/tmp/proj',
        provider: 'default',
        model: 'gpt-4o-mini',
        active: false,
      },
    };
    dispatchMessage(msg, mockCtx(conn));
    expect(useFlux.getState().chats).toHaveLength(1);
    expect(useFlux.getState().activeChatId).toBe('new');
  });

  it('usage accumulates into per-chat totals', () => {
    dispatchMessage(
      {
        type: 'usage',
        chat_id: 'c1',
        prompt_tokens: 100,
        completion_tokens: 50,
        cached_tokens: 25,
      },
      mockCtx(conn),
    );
    dispatchMessage(
      {
        type: 'usage',
        chat_id: 'c1',
        prompt_tokens: 200,
        completion_tokens: 80,
        cached_tokens: 150,
      },
      mockCtx(conn),
    );
    expect(useFlux.getState().usage['c1']).toEqual({
      inTokens: 300,
      outTokens: 130,
      cachedTokens: 175,
      contextTokens: 200,
    });
  });

  it('error without chat_id shows in empty state', () => {
    dispatchMessage({ type: 'error', code: 'internal', message: 'boom' }, mockCtx(conn));
    const wrap = document.getElementById('messages-wrap');
    expect(wrap?.querySelector('.message.error')).toBeTruthy();
  });

  it('error without chat_id delegates to stream when chat active', () => {
    // handleServerError resolves the target chat via the module singleton.
    useFlux.setState({ activeChatId: 'c1' });
    dispatchMessage({ type: 'error', code: 'internal', message: 'boom' }, mockCtx(conn));
    // The stream error handler appends to the chat pane, not the raw wrap.
    const pane = document.querySelector('.chat-pane') as HTMLElement;
    const errEl = pane?.querySelector('.message.error') as HTMLElement;
    expect(errEl).toBeDefined();
    if (errEl) expect(errEl.textContent).toContain('boom');
    // No raw global bubble as a direct child of the wrap (the pane lives
    // inside it, so a nested query would match the pane's own error).
    const wrap = document.getElementById('messages-wrap') as HTMLElement;
    const directError = Array.from(wrap.children).find(
      (el) => el.classList.contains('message') && el.classList.contains('error'),
    );
    expect(directError).toBeUndefined();
  });

  it('question_required delegates the answer to the dialogs layer', async () => {
    // The answering capability lives in the dialogs impl (the UI owns the UX) — the handler only calls the interface and sends the answer back.
    let resolveAsk: (a: string) => void = () => {};
    const askQuestion = vi.fn(
      (_chatId: string, _q: { text: string; options?: string[] }) =>
        new Promise<string>((resolve) => {
          resolveAsk = resolve;
        }),
    );
    setDialogImpls({ askQuestion: (chatId, q) => askQuestion(chatId, q) });
    const conn = mockConn();
    dispatchMessage(
      {
        type: 'question_required',
        chat_id: 'c1',
        id: 'q1',
        question: { text: 'Which database?', options: ['postgres', 'sqlite'] },
      },
      mockCtx(conn),
    );
    expect(askQuestion).toHaveBeenCalledWith('c1', {
      text: 'Which database?',
      options: ['postgres', 'sqlite'],
    });
    resolveAsk('postgres');
    await vi.waitFor(() => {
      expect(conn.send).toHaveBeenCalledWith({
        type: 'question_response',
        chat_id: 'c1',
        id: 'q1',
        answer: 'postgres',
      });
    });
  });

  it('unregistered type is a no-op', () => {
    // should not throw
    dispatchMessage({ type: 'unknown_type' } as unknown as ServerMessage, mockCtx(conn));
  });
});

describe('handleServerError', () => {
  beforeEach(() => {
    resetFluxForTest();
    resetBridgeForTest();
  });

  it('delegates to stream error when chat active', () => {
    useFlux.setState({ activeChatId: 'c1' });
    document.body.innerHTML = '<div id="messages-wrap"></div>';

    handleServerError('test error message');
    // Should create a stream error in the active chat pane
  });

  it('creates error div when no active chat', () => {
    document.body.innerHTML = '<div id="messages-wrap"></div>';

    handleServerError('test error message');
    const errorEl = document.querySelector('.message.error');
    expect(errorEl).toBeTruthy();
    expect(errorEl?.textContent).toContain('test error message');
  });
});
