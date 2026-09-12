/**
 * grpc-connection.ts — the event-plane connection manager over Connect.
 *
 * ONE session-scoped Subscribe stream per page anchors the identity: the
 * stream open attaches (or adopts via the stored token), the first `ready`
 * element carries the authoritative token + leases, the stream close
 * detaches into the server's grace window. Keepalive elements mark the
 * connection live — a frame deadline (3× the server's 30s period) detects
 * a half-open connection and reconnects (the WS ping/pong role).
 *
 * The stream elements are translated ONTO the existing handler vocabulary
 * (ServerMessage shapes dispatched through services/dispatch.ts), so the
 * whole handler table and its tests keep working over the new wire. The
 * R2 sequence reconciles stream elements against the claim snapshots:
 * chat_history/chat_state elements record the snapshot's per-chat seq;
 * content elements strictly BELOW it are pre-snapshot replays and are
 * dropped (the equal one is the first live element after the snapshot).
 *
 * The reverse direction (`send`) translates the ClientMessage vocabulary
 * onto the ChatService RPCs, synthesizing the error frames the lease-gate
 * statuses map to — the handler table never learns the transport.
 *
 * Provides: ConnectConnection, elementToFrame, reconcileElement
 * Depends: core/grpc.ts, core/session.ts, core/types.ts, logger.ts
 */

import { clients } from './grpc';
import { readStoredSessionId, storeSessionId } from './session';
import type {
  ChatInfo,
  ClientMessage,
  HistoryMessage,
  SavedModelInfo,
  ServerMessage,
} from './types';
import type { ChatInfo as ProtoChatInfo, Message as ProtoMessage } from '../gen/flux/v1/common_pb';
import type { SubscribeResponse } from '../gen/flux/v1/events_pb';
import { ChatKind as ProtoChatKind, ChatStateKind as ProtoChatStateKind } from '../gen/flux/v1/common_pb';
import { SkillSource as ProtoSkillSource } from '../gen/flux/v1/common_pb';
import { log } from '../logger';
import { newId } from '../lib/id';

/** Server keepalive period (grpc::events); the deadline is 3× so a
 * live-but-idle server always answers within it. */
const KEEPALIVE_DEADLINE_MS = 90_000;
/** Deadline check cadence — well under the deadline itself. */
const DEADLINE_TICK_MS = 10_000;
/** Reconnect backoff schedule (mirrors the old WS manager's). */
const RETRY_DELAYS_MS = [2_000, 4_000, 8_000, 16_000, 30_000];

export type ConnectionStatus = 'connecting' | 'connected' | 'disconnected' | 'failed';

// ── element → wire-frame translation (pure) ────────────────────────────────

/** proto ChatInfo → the wire ChatInfo the store/handlers consume. */
export function protoChatToChat(c: ProtoChatInfo): ChatInfo {
  return {
    chat_id: c.chatId,
    name: c.name,
    created_at: c.createdAt,
    last_activity_at: c.lastActivityAt,
    active: c.active,
    kind: c.kind === ProtoChatKind.FEATURE ? 'feature' : 'classic',
    workdir: c.workdir,
    provider: c.provider,
    model: c.model,
  };
}

/** proto Message → the wire HistoryMessage (role enum → wire string). */
export function protoMessageToHistory(m: ProtoMessage): HistoryMessage {
  const role =
    m.role === 1 ? 'system' : m.role === 2 ? 'user' : m.role === 3 ? 'assistant' : 'tool';
  return {
    // proto int64 → bigint; row ids sit far below 2^53. 0 = not persisted.
    id: m.id ? Number(m.id) : undefined,
    role,
    content: m.content,
    reasoning_content: m.reasoningContent || undefined,
    tool_calls: m.toolCalls.length
      ? m.toolCalls.map((t) => ({ id: t.id, name: t.name, arguments: t.arguments }))
      : undefined,
    tool_call_id: m.toolCallId || undefined,
  };
}

/**
 * One SubscribeResponse element → the ServerMessage shape the handler
 * table dispatches, or null when the element carries no frame (keepalive)
 * or cannot be represented. `ready` is handled by the connection itself
 * (identity bookkeeping + the session_resumed synthesis) and never passes
 * through here.
 */
export function elementToFrame(el: SubscribeResponse): ServerMessage | null {
  const chatId = el.chatId;
  const k = el.kind;
  if (!k) return null;
  switch (k.case) {
    case 'textDelta':
      return { type: 'text_delta', chat_id: chatId, delta: k.value.delta };
    case 'reasoningDelta':
      return { type: 'reasoning_delta', chat_id: chatId, delta: k.value.delta };
    case 'usage':
      return {
        type: 'usage',
        chat_id: chatId,
        prompt_tokens: k.value.promptTokens,
        completion_tokens: k.value.completionTokens,
        cached_tokens: k.value.cachedTokens,
      };
    case 'toolStart':
      return {
        type: 'tool_start',
        chat_id: chatId,
        id: k.value.id,
        name: k.value.name,
        arguments: k.value.arguments,
      };
    case 'toolCallPreview':
      return {
        type: 'tool_preview',
        chat_id: chatId,
        id: k.value.id,
        name: k.value.name ?? undefined,
        arguments_delta: k.value.argumentsDelta ?? undefined,
      };
    case 'toolResult':
      return { type: 'tool_result', chat_id: chatId, id: k.value.id, result: k.value.result };
    case 'questionRequired':
      return {
        type: 'question_required',
        chat_id: chatId,
        id: k.value.id,
        question: {
          text: k.value.question?.text ?? '',
          options: k.value.question?.options.length ? k.value.question.options : undefined,
        },
      };
    case 'streamEnd':
      return { type: 'stream_end', chat_id: chatId, finish_reason: k.value.finishReason ?? undefined };
    case 'streamCancelled':
      return { type: 'stream_cancelled', chat_id: chatId };
    case 'chatState':
      return {
        type: 'chat_state',
        chat_id: chatId,
        state: k.value.state === ProtoChatStateKind.STREAMING ? 'streaming' : 'idle',
      };
    case 'chatHistory':
      return {
        type: 'chat_history',
        chat_id: chatId,
        messages: k.value.messages.map(protoMessageToHistory),
      };
    case 'contextRebased': {
      // proto int64 → bigint; message ids sit far below 2^53.
      const base = k.value.baseMessageId;
      return {
        type: 'context_rebased',
        chat_id: chatId,
        base_message_id: typeof base === 'bigint' ? Number(base) : 0,
      };
    }
    case 'providerSwitched':
      return {
        type: 'provider_switched',
        chat_id: chatId,
        provider: k.value.provider,
        model: k.value.model,
      };
    case 'error':
      return {
        type: 'error',
        chat_id: chatId || undefined,
        code: protoErrorCodeToWire(k.value.code),
        message: k.value.message,
      };
    case 'chats':
      return { type: 'chats', chats: k.value.chats.map(protoChatToChat) };
    case 'chatCreated':
      return { type: 'chat_created', chat: protoChatToChat(k.value.chat!) };
    case 'providers':
      return {
        type: 'providers',
        providers: k.value.providers.map((p) => ({ id: p.id, url: p.url })),
      };
    case 'models':
      return {
        type: 'models',
        models: k.value.models.map(
          (m): SavedModelInfo => ({
            provider: m.provider,
            model: m.model,
            params: JSON.parse(m.paramsJson || '{}'),
            meta: JSON.parse(m.metaJson || '{}'),
          }),
        ),
      };
    case 'mcpServers':
      return {
        type: 'mcp_servers',
        servers: k.value.servers.map((s) => ({
          id: s.id,
          command: s.command,
          args: s.args,
          env_keys: s.envKeys,
        })),
      };
    case 'skills':
      return {
        type: 'skills',
        skills: k.value.skills.map((s) => ({
          name: s.name,
          description: s.description,
          source: (s.source === ProtoSkillSource.PROJECT ? 'project' : 'global') as
            | 'global'
            | 'project',
          removable: s.removable,
        })),
      };
    default:
      // ready/keepalive never reach here (handled by the connection loop).
      return null;
  }
}

/** proto ErrorCode enum → the wire code string (internal for unknown). */
function protoErrorCodeToWire(code: number): Extract<
  ServerMessage,
  { type: 'error' }
>['code'] {
  switch (code) {
    case 1:
      return 'provider_connection';
    case 2:
      return 'tool_execution';
    case 3:
      return 'invalid_arguments';
    case 4:
      return 'chat_busy';
    case 5:
      return 'chat_not_found';
    case 6:
      return 'stream_crashed';
    case 7:
      return 'stream_gap';
    case 8:
      return 'invalid_request';
    default:
      return 'internal';
  }
}

// ── the R2 snapshot reconciliation (pure) ──────────────────────────────────

/** The stream-content kinds the snapshot reconciliation gates. Errors,
 * questions, snapshots, and session-level broadcasts are exempt (their
 * delivery is not ordering-sensitive, and a re-delivered question IS the
 * delivery mechanism). */
const GATED_KINDS = new Set([
  'textDelta',
  'reasoningDelta',
  'usage',
  'toolStart',
  'toolCallPreview',
  'toolResult',
  'streamEnd',
  'streamCancelled',
  'chatState',
  'contextRebased',
  'providerSwitched',
]);

/**
 * The R2 gate: `snapshotSeq` maps chat_id → the last claim/open snapshot's
 * seq (recorded from chat_history/chat_state elements). A gated content
 * element with seq STRICTLY BELOW the snapshot's predates the snapshot —
 * its effect is already in it — and is dropped; the equal one is the
 * first live element after it (the snapshot peeks the counter the next
 * consumed element also returns). Returns true when the element must be
 * dropped. `snapshotSeq` is mutated: chat_history/chat_state record.
 */
export function reconcileElement(el: SubscribeResponse, snapshotSeq: Record<string, number>): boolean {
  const k = el.kind;
  const chatId = el.chatId;
  if (!k || !chatId) return false;
  if (k.case === 'chatHistory' || k.case === 'chatState') {
    // The snapshot slots the client into the ordering.
    snapshotSeq[chatId] = Number(el.chatSeq);
    return false;
  }
  if (k.case === undefined || !GATED_KINDS.has(k.case)) return false;
  const snap = snapshotSeq[chatId];
  if (snap === undefined) return false;
  return Number(el.chatSeq) < snap;
}

// ── the connection manager ─────────────────────────────────────────────────

type MessageHandler = (msg: ServerMessage) => void;

/**
 * ConnectConnection mirrors the old WS ConnectionManager's contract:
 * status transitions, an open handler, a message handler, a pending queue
 * flushed on open, exponential reconnect backoff, and half-open detection
 * (now keyed on the server's keepalive frames instead of pongs). The send
 * path translates ClientMessages onto ChatService RPCs.
 */
export class ConnectConnection {
  private abort: AbortController | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private deadlineTimer: ReturnType<typeof setTimeout> | null = null;
  private lastActivity = 0;
  /** Per-chat snapshot seqs (the R2 reconciliation state). */
  private snapshotSeq: Record<string, number> = {};
  private pendingQueue: ClientMessage[] = [];
  private retryCount = 0;
  private connecting = false;
  private disposed = false;
  private onMessage: MessageHandler | null = null;
  private onStatusChange: ((s: ConnectionStatus) => void) | null = null;
  private onOpenHandler: (() => void) | null = null;

  setMessageHandler(handler: MessageHandler): void {
    this.onMessage = handler;
  }

  setStatusHandler(handler: (s: ConnectionStatus) => void): void {
    this.onStatusChange = handler;
  }

  /** Invoked after the stream attaches (initial connect and every reconnect). */
  setOpenHandler(handler: () => void): void {
    this.onOpenHandler = handler;
  }

  connect(): void {
    if (this.connecting || this.disposed) return;
    this.connecting = true;
    this.onStatusChange?.('connecting');
    this.teardownStream();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    const abort = new AbortController();
    this.abort = abort;
    // Identity continuity (D-19): the stored token rides the open — the
    // adoption happens IN the handshake (the ready frame always carries
    // the authoritative result).
    const prevSession = readStoredSessionId();
    void this.runStream(abort, prevSession ?? undefined);
  }

  /** Consume the stream until it ends; status + reconnect around it. */
  private async runStream(abort: AbortController, resumeToken: string | undefined): Promise<void> {
    try {
      const options = { signal: abort.signal, timeoutMs: undefined };
      // The streaming call resolves to the async iterable (connect-es).
      const stream = await clients.events.subscribe({ sessionId: resumeToken }, options);
      for await (const el of stream) {
        this.lastActivity = Date.now();
        if (el.kind.case === 'ready') {
          this.onReady(el);
          continue;
        }
        if (el.kind.case === 'keepalive') continue; // liveness only
        // One malformed element must never kill the stream: translation
        // failures (e.g. a bad embedded JSON) are logged and skipped — a
        // throw here would tear down the whole event plane.
        try {
          if (reconcileElement(el, this.snapshotSeq)) {
            log.debug('dropped pre-snapshot element (R2): ' + el.kind.case);
            continue;
          }
          const frame = elementToFrame(el);
          if (frame) this.onMessage?.(frame);
        } catch (err) {
          log.warn(
            'dropped untranslatable element (' +
              el.kind.case +
              '): ' +
              (err instanceof Error ? err.message : String(err)),
          );
        }
      }
      // Clean server-side end — treat as a disconnect and reconnect.
      this.handleDisconnect('stream ended');
    } catch (err) {
      if (abort.signal.aborted) return; // superseded — stay quiet
      this.handleDisconnect(err instanceof Error ? err.message : String(err));
    }
  }

  /** The handshake: adopt-or-mint result + leases. */
  private onReady(el: SubscribeResponse): void {
    this.connecting = false;
    this.retryCount = 0;
    this.snapshotSeq = {}; // a fresh stream restarts the ordering slot-in
    if (el.kind.case !== 'ready') return; // guarded by the caller
    const ready = el.kind.value;
    storeSessionId(ready.sessionId);
    // The WS path's session_resumed semantics, reused verbatim: the
    // authoritative identity + the leases restore the focus and pull the
    // chat list. Synthesizing the frame keeps the handler table the ONE
    // place that logic lives.
    this.startDeadlineCheck();
    log.info('stream attached (session ' + ready.sessionId + ')');
    this.onStatusChange?.('connected');
    // Reconnect reset runs BEFORE the chats handler's re-open check.
    try {
      this.onOpenHandler?.();
    } catch (err) {
      log.error('open handler failed: ' + (err instanceof Error ? err.message : String(err)));
    }
    this.onMessage?.({
      type: 'session_resumed',
      session_id: ready.sessionId,
      leases: ready.leases,
    });
    // Flush the messages queued during the outage — SEQUENTIALLY: each
    // RPC is an independent HTTP request and their arrival order is not
    // guaranteed, but queued user turns are order-sensitive (the R1
    // interject discipline depends on it).
    void this.flushPending();
  }

  /** Drain the pending queue in order (awaited per send). */
  private async flushPending(): Promise<void> {
    while (this.pendingQueue.length > 0 && !this.disposed) {
      const d = this.pendingQueue.shift()!;
      await this.sendNow(d);
    }
  }

  private handleDisconnect(reason: string): void {
    if (this.disposed) return;
    this.connecting = false;
    this.stopDeadlineCheck();
    this.retryCount++;
    const delay = RETRY_DELAYS_MS[Math.min(this.retryCount - 1, RETRY_DELAYS_MS.length - 1)];
    log.info('stream detached (' + reason + ') — retry ' + this.retryCount + ' in ' + delay + 'ms');
    this.onStatusChange?.('disconnected');
    if (this.retryCount <= RETRY_DELAYS_MS.length) {
      this.reconnectTimer = setTimeout(() => this.connect(), delay);
    } else {
      // Give up auto-retry, but KEEP the pending queue: messages sent
      // during the outage must not be silently destroyed. The UI shows
      // 'failed' and the user can reconnect manually to flush the queue.
      this.onStatusChange?.('failed');
    }
  }

  /** Half-open detection: the server's keepalives must keep arriving. */
  private startDeadlineCheck(): void {
    this.stopDeadlineCheck();
    this.deadlineTimer = setInterval(() => {
      if (Date.now() - this.lastActivity > KEEPALIVE_DEADLINE_MS) {
        log.warn('frame deadline exceeded — reconnecting');
        this.reconnect();
      }
    }, DEADLINE_TICK_MS);
  }

  private stopDeadlineCheck(): void {
    if (this.deadlineTimer) {
      clearInterval(this.deadlineTimer);
      this.deadlineTimer = null;
    }
  }

  /**
   * Send a ClientMessage. Queues while not connected (a cancel targets a
   * live round and rounds die with the connection — dropped instead).
   * Connected messages translate onto the ChatService RPCs; lease-gate
   * refusals synthesize the error frames the handler table already knows.
   */
  send(data: ClientMessage): void {
    if (!this.abort || this.connecting) {
      if (data.type === 'cancel') {
        log.debug('send dropped (stream not attached): cancel cannot reach a live round');
        return;
      }
      log.debug('send queued (stream state): ' + data.type);
      this.pendingQueue.push(data);
      return;
    }
    log.info('send: ' + data.type);
    void this.sendNow(data);
  }

  private async sendNow(data: ClientMessage): Promise<void> {
    const token = readStoredSessionId() ?? undefined;
    const headers: Record<string, string> = token ? { 'x-flux-session': token } : {};
    try {
      switch (data.type) {
        case 'chat_list': {
          const resp = await clients.chat.listChats({}, { timeoutMs: 8000, headers });
          this.onMessage?.({ type: 'chats', chats: resp.chats.map(protoChatToChat) });
          return;
        }
        case 'chat_create': {
          const resp = await clients.chat.createChat(
            {
              name: data.name,
              workdir: data.workdir,
              kind: data.kind === 'feature' ? 2 : 1,
              provider: data.provider,
              model: data.model,
            },
            { timeoutMs: 8000, headers },
          );
          if (resp.error) {
            this.onMessage?.({ type: 'error', code: 'invalid_request', message: resp.error });
          } else if (resp.chat) {
            // The ack IS the chat_created payload (the broadcast rides the
            // stream as the fresh chats list).
            this.onMessage?.({ type: 'chat_created', chat: protoChatToChat(resp.chat) });
          }
          return;
        }
        case 'chat': {
          // One idempotency key per user intent: if the ack is lost to an
          // ambiguous failure (timeout / transport), retry ONCE with the
          // SAME key — the server dedups, so a landed first attempt is
          // absorbed as a duplicate instead of running the turn twice.
          // Definitive statuses (busy / not_found / ...) never retry.
          const req = {
            chatId: data.chat_id,
            message: data.message,
            // interrupt = the fused cancel+send (R1): the server pairs them
            // on one kernel FIFO, so no client-side park/flush is needed.
            interrupt: data.interrupt === true,
            clientMsgId: newId(),
          };
          const opts = { timeoutMs: 8000, headers };
          try {
            await clients.chat.sendMessage(req, opts);
          } catch (err) {
            const code = (err as { code?: number }).code;
            // 4 = DEADLINE_EXCEEDED, 14 = UNAVAILABLE — ambiguous.
            if (code === 4 || code === 14) {
              log.info('send ack lost — retrying with the same idempotency key');
              await clients.chat.sendMessage(req, opts);
            } else {
              throw err;
            }
          }
          return;
        }
        case 'cancel': {
          await clients.chat.cancelRound(
            { chatId: data.chat_id },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        case 'chat_claim': {
          // (The optimistic read-only clear + loadedChatId mark stay with
          // the callers — lease.ts — same as the WS path.) A claim always
          // grants: another holder's lease is STOLEN server-side and the
          // previous holder is demoted in-band (the chat_busy error event
          // degrades ITS pane). The snapshot rides the stream (single-point
          // delivery).
          await clients.chat.claimChat(
            { chatId: data.chat_id },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        case 'chat_open': {
          await clients.chat.openChat(
            { chatId: data.chat_id },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        case 'chat_close': {
          await clients.chat.closeChat(
            { chatId: data.chat_id },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        case 'chat_delete': {
          await clients.chat.deleteChat(
            { chatId: data.chat_id },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        case 'chat_rename': {
          await clients.chat.renameChat(
            { chatId: data.chat_id, name: data.name },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        case 'chat_provider': {
          // Validation failures (unknown provider id) ride the response's
          // inline error (D4'); the lease gate rides statuses. The swap
          // applies at the round boundary, `provider_switched` announces it.
          const resp = await clients.chat.switchProvider(
            { chatId: data.chat_id, provider: data.provider, model: data.model },
            { timeoutMs: 8000, headers },
          );
          if (resp.error) {
            this.onMessage?.({
              type: 'error',
              chat_id: data.chat_id,
              code: 'invalid_request',
              message: resp.error,
            });
          }
          return;
        }
        case 'rebase': {
          const resp = await clients.chat.rebaseChat(
            {
              chatId: data.chat_id,
              baseMessageId:
                data.base_message_id !== undefined ? BigInt(data.base_message_id) : undefined,
            },
            { timeoutMs: 8000, headers },
          );
          // The echoed base is informational — the context_rebased stream
          // event (same value) is the notice the pane renders.
          log.info('rebase [' + data.chat_id + '] landed at base ' + resp.baseMessageId);
          return;
        }
        case 'question_response': {
          // Stale/unknown answers drop silently server-side — same as the
          // WS frame; a refusal surfaces no error.
          await clients.chat.answerQuestion(
            { chatId: data.chat_id, id: data.id, answer: data.answer },
            { timeoutMs: 8000, headers },
          );
          return;
        }
        default:
          // Management/fs families ride their own typed clients (services/
          // *.ts call core/grpc.ts directly); ping/session_resume have no
          // Connect equivalent (keepalives anchor liveness and identity).
          log.debug('send ignored on the connect plane: ' + data.type);
          return;
      }
    } catch (err) {
      if (this.disposed) return;
      const status = err as { code?: number; message?: string; rawMessage?: string };
      const chatId = 'chat_id' in data ? (data as { chat_id: string }).chat_id : undefined;
      // Lease-gate refusals → the error frames the handler table knows.
      if (status.code === 9) {
        this.onMessage?.({
          type: 'error',
          chat_id: chatId,
          code: 'chat_busy',
          message: 'Chat is in use by another session',
        });
        return;
      }
      if (status.code === 5) {
        this.onMessage?.({
          type: 'error',
          chat_id: chatId,
          code: 'chat_not_found',
          message: 'Chat not found' + (chatId ? `: ${chatId}` : ''),
        });
        return;
      }
      // 3 = INVALID_ARGUMENT — request-scoped validation, not a crash.
      if (status.code === 3) {
        this.onMessage?.({
          type: 'error',
          chat_id: chatId,
          code: 'invalid_request',
          message: status.message ?? 'invalid request',
        });
        return;
      }
      // 16 = UNAUTHENTICATED — the identity is gone (e.g. a server restart
      // wiped it). An error bubble cannot fix it; re-anchor the identity
      // (the ready frame mints a fresh one) instead of rendering a crash.
      if (status.code === 16) {
        log.warn('unauthenticated on a control call — re-anchoring the identity');
        this.reconnect();
        return;
      }
      // Transport-level failure with the stream still "attached" — the
      // next element (or deadline) will detach us; surface as an error.
      this.onMessage?.({
        type: 'error',
        chat_id: chatId,
        code: 'internal',
        message: status.message ?? 'request failed',
      });
    }
  }

  reconnect(): void {
    this.retryCount = 0;
    this.connecting = false;
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.connect();
  }

  dispose(): void {
    this.disposed = true;
    this.stopDeadlineCheck();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    this.teardownStream();
    this.onMessage = null;
    this.onStatusChange = null;
    this.onOpenHandler = null;
    this.pendingQueue.length = 0;
    this.connecting = false;
  }

  private teardownStream(): void {
    if (this.abort) {
      this.abort.abort();
      this.abort = null;
    }
  }
}
