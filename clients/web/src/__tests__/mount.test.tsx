/**
 * mount.test.tsx — mountChat initialization tests (the Connect plane).
 *
 * Regression coverage for the refresh white-screen bug: restoring the
 * persisted active chat used to run BEFORE the App render; the
 * activeChatId subscription fires synchronously on registration
 * (@preact/signals semantics) and its switchToChat → getPane path threw
 * 'messages-wrap not found', killing render AND connect — a blank page on
 * every refresh that had a persisted active chat.
 *
 * Module singletons (state signals, the handler registry, the bridge) are
 * reset per test via vi.resetModules + dynamic re-import — subscriptions
 * registered by one test's mount must never observe another test's writes.
 * The Connect transport is mocked at the clients surface: the event stream
 * stays open (never yields), the chat control calls are recorded — the
 * lease handover's close/claim pair is what the assertions pin.
 *
 * Provides: mount flow tests (render-first ordering, lease handover on restore)
 * Depends: ../mount, ../core/state, ../core/grpc (dynamically imported, reset per test)
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';

type MountModule = typeof import('../mount');
type StateModule = typeof import('../core/state');

/** The recorded chat-control calls of the mocked ChatService client. */
const chatCalls: { method: string; req: unknown }[] = [];

async function mockTransport() {
  const { create } = await import('@bufbuild/protobuf');
  const { SubscribeResponseSchema, ReadySchema } = await import('../gen/flux/v1/events_pb');
  const grpc = (await import('../core/grpc')) as unknown as {
    clients: Record<string, unknown>;
  };
  const events = {
    subscribe: async () =>
      (async function* () {
        // The handshake arrives (ready → the queue flushes), then the
        // stream stays open for the test.
        yield create(SubscribeResponseSchema, {
          chatId: '',
          chatSeq: 0n,
          kind: { case: 'ready', value: create(ReadySchema, { sessionId: 'sess-1', leases: [] }) },
        });
        await new Promise(() => {});
      })(),
  };
  const chat = new Proxy(
    {},
    {
      get(_t, method: string) {
        return async (req: unknown) => {
          chatCalls.push({ method, req });
          // The claim's oneof: granted with an empty payload; the list
          // carries its (empty) chats array.
          if (method === 'claimChat') return { outcome: { case: 'granted', value: {} } };
          if (method === 'listChats') return { chats: [] };
          return {};
        };
      },
    },
  );
  grpc.clients.events = events;
  grpc.clients.chat = chat;
}

describe('mountChat — refresh restore (regression: messages-wrap crash)', () => {
  let root: HTMLElement;
  let handle: import('../mount').MountHandle | null = null;
  let mountChat: MountModule['mountChat'];
  let useFlux: StateModule['useFlux'];
  let resetFluxForTest: StateModule['resetFluxForTest'];

  beforeEach(async () => {
    vi.resetModules();
    ({ mountChat } = await import('../mount'));
    ({ useFlux, resetFluxForTest } = await import('../core/state'));
    resetFluxForTest();

    sessionStorage.clear();
    document.body.innerHTML = '<div id="host"></div>';
    root = document.getElementById('host')!;
    chatCalls.length = 0;
    await mockTransport();
  });

  afterEach(() => {
    handle?.connection.dispose();
    handle = null;
    vi.useRealTimers();
    document.body.innerHTML = '';
    sessionStorage.clear();
    localStorage.clear();
  });

  it('restores the persisted active chat without killing render or connect', async () => {
    // Simulated refresh: the tab has an identity and an active chat from the
    // previous page life.
    sessionStorage.setItem('flux.session.id', 'sess-1');
    sessionStorage.setItem('flux.active.chat', 'chat-1');

    expect(() => {
      handle = mountChat({ root });
    }).not.toThrow();

    // Render completed: the host the old crash destroyed is present.
    expect(document.getElementById('messages-wrap')).not.toBeNull();
    // The restored chat's pane was created and made visible.
    expect(document.querySelector('[data-chat-id="chat-1"]')).not.toBeNull();

    // The identity rides the STREAM OPEN (the token in SubscribeRequest —
    // the adoption happens in the handshake; there is no separate resume
    // call). The restored focus's claim follows: the lease handover on
    // the chat-control client.
    await vi.waitFor(() => {
      expect(chatCalls.some((c) => c.method === 'claimChat')).toBe(true);
    });
    expect(chatCalls.some((c) => c.method === 'claimChat' && (c.req as { chatId: string }).chatId === 'chat-1')).toBe(true);

    // Sidebar collapse persists from the mount-level effect (P2).
    useFlux.setState({ sidebarOpen: false });
    expect(localStorage.getItem('flux.sidebar.open')).toBe('0');
  });

  it('first visit (no persisted chat) mounts without an early claim', async () => {
    expect(() => {
      handle = mountChat({ root });
    }).not.toThrow();

    expect(document.getElementById('messages-wrap')).not.toBeNull();

    // Give the mount-time flows a beat: no active chat → nothing to claim.
    // The claim only ever fires once the chats list names a chat.
    await new Promise((r) => setTimeout(r, 20));
    expect(chatCalls.some((c) => c.method === 'claimChat')).toBe(false);
  });

  it('a switchToChat DOM failure is contained (guarded subscription)', async () => {
    sessionStorage.setItem('flux.active.chat', 'chat-1');
    handle = mountChat({ root });
    expect(document.getElementById('messages-wrap')).not.toBeNull();

    // Break the wrap out from under the pipeline: the NEXT switch hits a
    // missing #messages-wrap and throws inside the subscription callback.
    document.getElementById('messages-wrap')?.remove();

    expect(() => {
      useFlux.setState({ activeChatId: 'chat-2' });
    }).not.toThrow();

    // The cursor advanced before the work: exactly ONE chat-2 claim (no
    // replay), and the handover progressed — chat-1 was closed. Only the
    // DOM switch failed, contained to a log line.
    await vi.waitFor(() => {
      const claims = chatCalls.filter(
        (c) => c.method === 'claimChat' && (c.req as { chatId: string }).chatId === 'chat-2',
      );
      expect(claims).toHaveLength(1);
    });
    expect(chatCalls.some((c) => c.method === 'closeChat' && (c.req as { chatId: string }).chatId === 'chat-1')).toBe(true);
  });
});
