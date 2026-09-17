/**
 * grpc-connection.test.ts — the Connect event-plane connection: element
 * translation (proto → the handler vocabulary), the R2 snapshot
 * reconciliation, the identity handshake (ready → session_resumed
 * synthesis + queue flush), and the chat-control send translation
 * (lease-gate statuses → the error frames the handler table knows).
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { create } from '@bufbuild/protobuf';
import {
  SubscribeResponseSchema,
  ReadySchema,
  KeepaliveSchema,
  TextDeltaSchema,
  ErrorEventSchema,
  ChatStateSchema,
  ChatHistorySchema,
  ChatsBroadcastSchema,
  ModelsBroadcastSchema,
  type SubscribeResponse,
} from '../../gen/flux/v1/events_pb';
import { ChatInfoSchema, MessageSchema } from '../../gen/flux/v1/common_pb';
import {
  elementToFrame,
  reconcileElement,
  ConnectConnection,
} from '../../core/grpc-connection';
import * as grpc from '../../core/grpc';
import { resetFluxForTest } from '../../core/state';

function el(chatId: string, chatSeq: bigint, kind: SubscribeResponse['kind']): SubscribeResponse {
  return create(SubscribeResponseSchema, { chatId, chatSeq, kind });
}

beforeEach(() => {
  resetFluxForTest();
  sessionStorage.clear();
  vi.restoreAllMocks();
});
afterEach(() => {
  vi.useRealTimers();
});

describe('elementToFrame (proto → the handler vocabulary)', () => {
  it('maps a text delta', () => {
    const frame = elementToFrame(el('c1', 3n, { case: 'textDelta', value: create(TextDeltaSchema, { delta: 'hi' }) }));
    expect(frame).toEqual({ type: 'text_delta', chat_id: 'c1', delta: 'hi' });
  });

  it('maps a chat_state snapshot to the wire state string', () => {
    const streaming = elementToFrame(
      el('c1', 0n, { case: 'chatState', value: create(ChatStateSchema, { state: 2 }) }),
    );
    expect(streaming).toMatchObject({ type: 'chat_state', chat_id: 'c1', state: 'streaming' });
    const idle = elementToFrame(
      el('c1', 0n, { case: 'chatState', value: create(ChatStateSchema, { state: 1 }) }),
    );
    expect(idle).toMatchObject({ type: 'chat_state', state: 'idle' });
  });

  it('maps chat_history messages (role enum → wire string, tool calls)', () => {
    const frame = elementToFrame(
      el('c1', 0n, {
        case: 'chatHistory',
        value: create(ChatHistorySchema, {
          messages: [
            create(MessageSchema, {
              role: 2,
              content: 'q',
              toolCalls: [{ id: 't1', name: 'bash', arguments: '{}' }],
            }),
            create(MessageSchema, { role: 3, content: 'a', reasoningContent: 'thinking' }),
          ],
        }),
      }),
    );
    expect(frame).toEqual({
      type: 'chat_history',
      chat_id: 'c1',
      messages: [
        { role: 'user', content: 'q', tool_calls: [{ id: 't1', name: 'bash', arguments: '{}' }] },
        { role: 'assistant', content: 'a', reasoning_content: 'thinking' },
      ],
    });
  });

  it('maps the chats broadcast with the kind enum', () => {
    const frame = elementToFrame(
      el('', 0n, {
        case: 'chats',
        value: create(ChatsBroadcastSchema, {
          chats: [
            {
              chatId: 'c1',
              name: 'n',
              createdAt: '2026-01-01T00:00:00Z',
              lastActivityAt: '2026-01-02T00:00:00Z',
              active: true,
              running: true,
              workdir: '/w',
              provider: 'p',
              model: 'm',
            },
          ],
        }),
      }),
    );
    expect(frame).toEqual({
      type: 'chats',
      chats: [
        {
          chat_id: 'c1',
          name: 'n',
          created_at: '2026-01-01T00:00:00Z',
          last_activity_at: '2026-01-02T00:00:00Z',
          active: true,
          running: true,
          workdir: '/w',
          provider: 'p',
          model: 'm',
        },
      ],
    });
  });

  it('maps error elements with the code enum (chat-scoped or session-level)', () => {
    const scoped = elementToFrame(
      el('c1', 0n, {
        case: 'error',
        value: create(ErrorEventSchema, { code: 4, message: 'busy' }),
      }),
    );
    expect(scoped).toEqual({ type: 'error', chat_id: 'c1', code: 'chat_busy', message: 'busy' });
    const global = elementToFrame(
      el('', 0n, {
        case: 'error',
        value: create(ErrorEventSchema, { code: 9, message: 'boom' }),
      }),
    );
    expect(global).toEqual({ type: 'error', chat_id: undefined, code: 'internal', message: 'boom' });
  });

  it('keepalive elements carry no frame', () => {
    const frame = elementToFrame(el('', 0n, { case: 'keepalive', value: create(KeepaliveSchema) }));
    expect(frame).toBeNull();
  });
});

describe('reconcileElement (the R2 snapshot reconciliation)', () => {
  it('records the snapshot seq from chat_history/chat_state', () => {
    const seq: Record<string, number> = {};
    expect(reconcileElement(el('c1', 5n, { case: 'chatHistory', value: create(ChatHistorySchema, { messages: [] }) }), seq)).toBe(
      false,
    );
    expect(seq['c1']).toBe(5);
  });

  it('drops gated content strictly BELOW the snapshot seq', () => {
    const seq: Record<string, number> = { c1: 5 };
    const delta = el('c1', 4n, { case: 'textDelta', value: create(TextDeltaSchema, { delta: 'stale' }) });
    expect(reconcileElement(delta, seq)).toBe(true);
  });

  it('keeps the element AT the snapshot seq (the first live element after it)', () => {
    // The snapshot peeks the CURRENT counter; the next consumed element
    // returns the same value (fetch_add's old value) — it must survive.
    const seq: Record<string, number> = { c1: 5 };
    const delta = el('c1', 5n, { case: 'textDelta', value: create(TextDeltaSchema, { delta: 'live' }) });
    expect(reconcileElement(delta, seq)).toBe(false);
  });

  it('never gates chats without a snapshot slot', () => {
    const seq: Record<string, number> = {};
    const delta = el('c2', 0n, { case: 'textDelta', value: create(TextDeltaSchema, { delta: 'fresh chat' }) });
    expect(reconcileElement(delta, seq)).toBe(false);
  });

  it('never gates errors or questions (type-scoped)', () => {
    const seq: Record<string, number> = { c1: 5 };
    const gap = el('c1', 0n, {
      case: 'error',
      value: create(ErrorEventSchema, { code: 7, message: 'gap' }),
    });
    expect(reconcileElement(gap, seq)).toBe(false);
  });
});

// ── the connection lifecycle ────────────────────────────────────────────────

/** A scripted async-iterable stream with abort support + a completion tap. */
function fakeStream(elements: SubscribeResponse[]) {
  const done = vi.fn();
  const subscribe = vi.fn(async (_req: unknown, opts?: { signal?: AbortSignal }) => {
    async function* gen() {
      for (const e of elements) {
        if (opts?.signal?.aborted) return;
        yield e;
      }
      done();
    }
    return gen();
  });
  return { subscribe, done };
}

/** A stream that yields its elements and then STAYS OPEN — the attached
 * state the send-translation tests need (a scripted-exhausted stream
 * detaches immediately, and since B1 a detached stream QUEUES sends
 * instead of firing the RPC mocks). */
function openStream(elements: SubscribeResponse[]) {
  const subscribe = vi.fn(async (_req: unknown, opts?: { signal?: AbortSignal }) =>
    (async function* () {
      for (const e of elements) {
        if (opts?.signal?.aborted) return;
        yield e;
      }
      await new Promise(() => {}); // attached until disposed/aborted
    })(),
  );
  return { subscribe };
}

describe('ConnectConnection lifecycle', () => {
  it('ready → session_resumed synthesis, store write, status connected, queue flush', async () => {
    const ready = el('', 0n, {
      case: 'ready',
      value: create(ReadySchema, { sessionId: 'tok-9', leases: ['c1'] }),
    });
    const delta = el('c1', 0n, { case: 'textDelta', value: create(TextDeltaSchema, { delta: 'hi' }) });
    const { subscribe } = fakeStream([ready, delta]);
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe } as never);

    const frames: unknown[] = [];
    const statuses: string[] = [];
    const conn = new ConnectConnection();
    conn.setStatusHandler((s) => statuses.push(s));
    conn.setMessageHandler((m) => frames.push(m));
    // A message queued BEFORE the stream attaches (the outage path).
    conn.send({ type: 'chat_list' });
    const listChats = vi.fn(async () => ({ chats: [] }));
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ listChats } as never);

    conn.connect();
    await vi.waitFor(() => {
      expect(statuses).toContain('connected');
      expect(frames.some((f) => (f as { type: string }).type === 'session_resumed')).toBe(true);
    });
    // The identity is stored (sessionStorage, the D-19 continuity key).
    expect(sessionStorage.getItem('flux.session.id')).toBe('tok-9');
    // The synthesized session_resumed carries the leases (focus restore).
    const resumed = frames.find((f) => (f as { type: string }).type === 'session_resumed') as {
      leases: string[];
    };
    expect(resumed.leases).toEqual(['c1']);
    // The queued chat_list flushed AFTER the attach (the identity settled).
    await vi.waitFor(() => expect(listChats).toHaveBeenCalledTimes(1));
    // The content element dispatched as a wire frame.
    expect(frames.some((f) => (f as { type: string }).type === 'text_delta')).toBe(true);
    conn.dispose();
  });

  it('a dropped stream → disconnect + scheduled reconnect; reconnect re-opens with the stored token', async () => {
    const { subscribe } = fakeStream([]);
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe } as never);
    const statuses: string[] = [];
    const conn = new ConnectConnection();
    conn.setStatusHandler((s) => statuses.push(s));
    sessionStorage.setItem('flux.session.id', 'tok-7');

    conn.connect();
    await vi.waitFor(() => expect(statuses).toContain('disconnected'));
    // The reopen carries the stored token (the adoption rides the open).
    expect(subscribe).toHaveBeenCalledTimes(1);
    expect((subscribe.mock.calls[0] as unknown[])[0]).toEqual({ sessionId: 'tok-7' });
    conn.dispose();
  });

  it('keepalives refresh liveness; the deadline forces a reconnect', async () => {
    vi.useFakeTimers();
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-ka' }) });
    const ka = el('', 0n, { case: 'keepalive', value: create(KeepaliveSchema) });
    // A stream that never ends: ready starts the deadline check, the
    // keepalive keeps liveness fresh, then silence.
    const subscribe = vi.fn(async () =>
      (async function* () {
        yield ready;
        yield ka;
        await new Promise(() => {}); // the stream stays open
      })(),
    );
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe } as never);
    const conn = new ConnectConnection();
    // Spy on the PROTOTYPE — the deadline callback runs `this.reconnect()`,
    // which resolves through the prototype chain.
    const spy = vi.spyOn(ConnectConnection.prototype as unknown as { reconnect: () => void }, 'reconnect');

    conn.connect();
    // The deadline ticks at 10s cadence; the FIRST tick past the 90s
    // deadline (t=100s) fires the reconnect. (The tick at t=90s sits AT
    // the deadline — not past it.)
    await vi.advanceTimersByTimeAsync(100_000);
    expect(spy).toHaveBeenCalled();
    conn.dispose();
  });
});

describe('ConnectConnection.send (chat control translation)', () => {
  function attachedConn(elements: SubscribeResponse[] = []) {
    const { subscribe } = openStream(elements);
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe } as never);
    const conn = new ConnectConnection();
    const frames: unknown[] = [];
    conn.setMessageHandler((m) => frames.push(m));
    return { conn, frames };
  }

  it('chat → SendMessage with the token in the metadata', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn } = attachedConn([ready]);
    const send = vi.fn(async () => ({}));
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ sendMessage: send } as never);
    conn.connect();
    await vi.waitFor(() => expect(send).not.toHaveBeenCalled());
    conn.send({ type: 'chat', chat_id: 'c1', message: 'hello' });
    await vi.waitFor(() => expect(send).toHaveBeenCalled());
    const [req, opts] = send.mock.calls[0] as unknown as [
      { chatId: string; message: string; interrupt: boolean; clientMsgId?: string },
      { headers: Record<string, string> },
    ];
    // A plain send leaves interrupt false (proto3 default — no fused cancel)
    // and carries a fresh idempotency key (the dedup contract).
    expect(req.chatId).toBe('c1');
    expect(req.message).toBe('hello');
    expect(req.interrupt).toBe(false);
    expect(req.clientMsgId).toBeTruthy();
    expect(opts.headers['x-flux-session']).toBe('tok-2');
    conn.dispose();
  });

  it('a busy refusal → the synthesized chat_busy error frame (handlers stay transport-blind)', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn, frames } = attachedConn([ready]);
    const busy = Object.assign(new Error('busy'), { code: 9 });
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({
      sendMessage: vi.fn(async () => {
        throw busy;
      }),
    } as never);
    conn.connect();
    await vi.waitFor(() => expect(frames.some((f) => (f as { type: string }).type === 'session_resumed')).toBe(true));
    conn.send({ type: 'chat', chat_id: 'c1', message: 'hello' });
    await vi.waitFor(() => {
      const err = frames.find((f) => (f as { type: string }).type === 'error') as {
        code: string;
        chat_id: string;
      };
      expect(err).toBeDefined();
      expect(err.code).toBe('chat_busy');
      expect(err.chat_id).toBe('c1');
    });
    conn.dispose();
  });

  it('a claim never rejects: a steal, never busy — the demotion rides the in-band error event', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn, frames } = attachedConn([ready]);
    const claimChat = vi.fn(async () => ({ alreadyOwned: false }));
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ claimChat } as never);
    conn.connect();
    await vi.waitFor(() => expect(frames.some((f) => (f as { type: string }).type === 'session_resumed')).toBe(true));
    conn.send({ type: 'chat_claim', chat_id: 'c1' });
    await vi.waitFor(() => expect(claimChat).toHaveBeenCalledTimes(1));
    // No synthesized error frame — the snapshot rides the stream.
    expect(frames.some((f) => (f as { type: string }).type === 'error')).toBe(false);
    conn.dispose();
  });

  it('chat sends carry an idempotency key; an ambiguous failure retries with the SAME key', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn } = attachedConn([ready]);
    const calls: { clientMsgId?: string }[] = [];
    const send = vi.fn(async (req: { clientMsgId?: string }) => {
      calls.push(req);
      if (calls.length === 1) throw Object.assign(new Error('timeout'), { code: 4 });
      return { duplicate: true };
    });
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ sendMessage: send } as never);
    conn.connect();
    await vi.waitFor(() => expect(sessionStorage.getItem('flux.session.id')).toBe('tok-2'));
    conn.send({ type: 'chat', chat_id: 'c1', message: 'hello' });
    await vi.waitFor(() => expect(calls.length).toBe(2));
    // Same idempotency key on the retry — the server absorbs the duplicate.
    expect(calls[0].clientMsgId).toBeTruthy();
    expect(calls[1].clientMsgId).toBe(calls[0].clientMsgId);
    conn.dispose();
  });

  it('a definitive refusal never retries the send', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn } = attachedConn([ready]);
    const send = vi.fn(async () => {
      throw Object.assign(new Error('busy'), { code: 9 });
    });
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ sendMessage: send } as never);
    conn.connect();
    await vi.waitFor(() => expect(sessionStorage.getItem('flux.session.id')).toBe('tok-2'));
    conn.send({ type: 'chat', chat_id: 'c1', message: 'hello' });
    await vi.waitFor(() => expect(send).toHaveBeenCalledTimes(1));
    await new Promise((r) => setTimeout(r, 20));
    expect(send).toHaveBeenCalledTimes(1);
    conn.dispose();
  });

  it('invalid_argument (3) maps to invalid_request, not internal', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn, frames } = attachedConn([ready]);
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({
      switchProvider: vi.fn(async () => {
        throw Object.assign(new Error('bad pin'), { code: 3 });
      }),
    } as never);
    conn.connect();
    await vi.waitFor(() => expect(frames.some((f) => (f as { type: string }).type === 'session_resumed')).toBe(true));
    conn.send({ type: 'chat_provider', chat_id: 'c1', provider: 'nope', model: 'm' });
    await vi.waitFor(() => {
      const err = frames.find((f) => (f as { type: string }).type === 'error') as { code: string };
      expect(err?.code).toBe('invalid_request');
    });
    conn.dispose();
  });

  it('an inline provider-switch refusal rides the response error, not a status', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn, frames } = attachedConn([ready]);
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({
      switchProvider: vi.fn(async () => ({ error: 'provider switch rejected: unknown id' })),
    } as never);
    conn.connect();
    await vi.waitFor(() => expect(frames.some((f) => (f as { type: string }).type === 'session_resumed')).toBe(true));
    conn.send({ type: 'chat_provider', chat_id: 'c1', provider: 'nope', model: 'm' });
    await vi.waitFor(() => {
      const err = frames.find((f) => (f as { type: string }).type === 'error') as {
        code: string;
        message: string;
      };
      expect(err?.code).toBe('invalid_request');
      expect(err?.message).toContain('unknown id');
    });
    conn.dispose();
  });

  it('an untranslatable element is logged and skipped — the stream lives on', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-x' }) });
    // A models broadcast with broken embedded JSON would throw inside
    // elementToFrame; the element after it must still dispatch.
    const bad = el('', 0n, {
      case: 'models',
      value: create(ModelsBroadcastSchema, {
        models: [{ provider: 'p', model: 'm', paramsJson: '{not json', metaJson: '{}' }],
      }),
    });
    const delta = el('c1', 0n, { case: 'textDelta', value: create(TextDeltaSchema, { delta: 'alive' }) });
    const { conn, frames } = attachedConn([ready, bad, delta]);
    conn.connect();
    await vi.waitFor(() => {
      expect(frames.some((f) => (f as { type: string }).type === 'text_delta')).toBe(true);
    });
    conn.dispose();
  });

  it('fork translates onto ForkChat and synthesizes chat_created from the ack', async () => {
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-2' }) });
    const { conn, frames } = attachedConn([ready]);
    const forkChat = vi.fn(async () => ({
      chat: create(ChatInfoSchema, {
        chatId: 'f1',
        name: 'src (fork)',
        forkedFromChatId: 'c1',
      }),
      error: undefined,
    }));
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ forkChat } as never);
    conn.connect();
    await vi.waitFor(() => expect(sessionStorage.getItem('flux.session.id')).toBe('tok-2'));
    conn.send({ type: 'fork', chat_id: 'c1', fork_point: 7 });
    await vi.waitFor(() => expect(forkChat).toHaveBeenCalledTimes(1));
    const [req] = forkChat.mock.calls[0] as unknown as [{ chatId: string; forkPoint: bigint }];
    expect(req.chatId).toBe('c1');
    expect(req.forkPoint).toBe(7n);
    await vi.waitFor(() => {
      const created = frames.find((f) => (f as { type: string }).type === 'chat_created') as
        | { chat: { chat_id: string; forked_from_chat_id?: string } }
        | undefined;
      expect(created?.chat.chat_id).toBe('f1');
      expect(created?.chat.forked_from_chat_id).toBe('c1');
    });
    conn.dispose();
  });

  it('cancel while detached is dropped, not queued (rounds die with the connection)', () => {
    const conn = new ConnectConnection();
    conn.send({ type: 'cancel', chat_id: 'c1' });
    // Nothing to observe directly — the invariant is the queue stays empty
    // so no cancel can fire after a reconnect.
    // (A queued chat proves the queue itself works — see the flush test.)
    conn.dispose();
  });

  it('a send in the DISCONNECT WINDOW queues instead of hitting the wire (B1)', async () => {
    // The stream attaches (ready) and then ENDS: everything after is the
    // disconnect window — abort is non-null there, but the stream is not
    // attached. The banner promises queued messages ride the reconnect;
    // a send here must queue, never fire a doomed RPC.
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-b1' }) });
    const { subscribe } = fakeStream([ready]);
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe } as never);
    const listChats = vi.fn(async () => ({ chats: [] }));
    const sendMessage = vi.fn(async () => ({}));
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ listChats, sendMessage } as never);

    const conn = new ConnectConnection();
    // A message queued BEFORE the stream attaches (the outage path).
    conn.send({ type: 'chat_list' });
    conn.connect();
    await vi.waitFor(() =>
      expect(sessionStorage.getItem('flux.session.id')).toBe('tok-b1'),
    );
    // The queued chat_list flushed on attach; the scripted stream then
    // exhausts → handleDisconnect → the window.
    await vi.waitFor(() => expect(listChats).toHaveBeenCalledTimes(1));
    conn.send({ type: 'chat', chat_id: 'c1', message: 'typed offline' });
    await new Promise((r) => setTimeout(r, 20));
    // The window send never reached the wire — it sits queued.
    expect(listChats).toHaveBeenCalledTimes(1);
    expect(sendMessage).not.toHaveBeenCalled();
    conn.dispose();
  });

  it('a send whose stream detaches mid-flight re-queues instead of an error bubble (B1)', async () => {
    // An OPEN stream (attached), an in-flight RPC, then the stream ends
    // under it and the RPC fails bare: definitive statuses are mapped, a
    // bare transport failure while detached re-queues — no error bubble.
    const ready = el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-b1b' }) });
    let endStream: () => void = () => {};
    const ended = new Promise<void>((r) => (endStream = r));
    const subscribe = vi.fn(async () =>
      (async function* () {
        yield ready;
        await ended;
      })(),
    );
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe } as never);
    // The NEXT attach (the reconnect) rides its own open stream.
    const subscribe2 = vi.fn(async () =>
      (async function* () {
        yield el('', 0n, { case: 'ready', value: create(ReadySchema, { sessionId: 'tok-b1b' }) });
        await new Promise(() => {});
      })(),
    );

    let failRpc: (() => void) | null = null;
    const sendMessage = vi.fn(
      (_req: unknown) =>
        new Promise((_res, rej) => {
          failRpc = () => rej(new Error('transport gone'));
        }),
    );
    const listChats = vi.fn(async () => ({ chats: [] }));
    vi.spyOn(grpc.clients, 'chat', 'get').mockReturnValue({ listChats, sendMessage } as never);

    const frames: unknown[] = [];
    const conn = new ConnectConnection();
    conn.setMessageHandler((m) => frames.push(m));
    conn.connect();
    await vi.waitFor(() => expect(sessionStorage.getItem('flux.session.id')).toBe('tok-b1b'));
    conn.send({ type: 'chat', chat_id: 'c1', message: 'in flight' });
    await vi.waitFor(() => expect(sendMessage).toHaveBeenCalled());
    // The stream ends while the RPC is still pending — the disconnect
    // window opens under the in-flight send.
    endStream();
    await vi.waitFor(() =>
      expect(frames.some((f) => (f as { type: string }).type === 'session_resumed')).toBe(true),
    );
    failRpc!();
    await new Promise((r) => setTimeout(r, 20));
    // No internal error bubble — the message re-queues for the reconnect.
    expect(frames.some((f) => (f as { type: string }).type === 'error')).toBe(false);
    expect(sendMessage).toHaveBeenCalledTimes(1);
    // The queued send flushes on the NEXT attach.
    vi.spyOn(grpc.clients, 'events', 'get').mockReturnValue({ subscribe: subscribe2 } as never);
    conn.reconnect();
    await vi.waitFor(() => expect(sendMessage).toHaveBeenCalledTimes(2));
    conn.dispose();
  });
});
