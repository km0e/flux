/**
 * grpc-connection.ts — the event-plane connection manager over Connect.
 *
 * ONE session-scoped Subscribe stream per page anchors the identity: the
 * stream open attaches (or adopts via the stored token), the first `ready`
 * element carries the authoritative token + leases, the stream close
 * detaches into the server's grace window. Keepalive elements mark the
 * connection live — a frame deadline (3× the server's 30s period) detects
 * a half-open connection and reconnects.
 *
 * This file owns the STATEFUL manager only: status transitions, the open
 * handler, the pending queue flushed on open, the exponential reconnect
 * backoff, and the keepalive failsafe. The pure translations live beside
 * it — element→frame + the R2 reconciliation in `grpc-frames.ts`, the
 * ClientMessage→RPC send path in `grpc-send.ts` (both re-exported here so
 * the module's public surface is unchanged).
 *
 * Provides: ConnectConnection; re-exports protoChatToChat,
 *           protoMessageToHistory, elementToFrame, reconcileElement
 * Depends: core/grpc.ts, core/session.ts, core/types.ts, core/grpc-frames.ts,
 *          core/grpc-send.ts, logger.ts
 */

import { clients } from './grpc';
import { readStoredSessionId, storeSessionId } from './session';
import type { ClientMessage, ServerMessage } from './types';
import type { SubscribeResponse } from '../gen/flux/v1/events_pb';
import { log } from '../logger';
import { elementToFrame, reconcileElement } from './grpc-frames';
import { sendClientMessage } from './grpc-send';

export { protoChatToChat, protoMessageToHistory, elementToFrame, reconcileElement } from './grpc-frames';

/** Server keepalive period (grpc::events); the deadline is 3× so a
 * live-but-idle server always answers within it. */
const KEEPALIVE_DEADLINE_MS = 90_000;
/** Deadline check cadence — well under the deadline itself. */
const DEADLINE_TICK_MS = 10_000;
/** Reconnect backoff schedule: 2s base, ×2 per retry, 30s cap, 5 tries. */
const RETRY_DELAYS_MS = [2_000, 4_000, 8_000, 16_000, 30_000];

export type ConnectionStatus = 'connecting' | 'connected' | 'disconnected' | 'failed';

// ── the connection manager ─────────────────────────────────────────────────

type MessageHandler = (msg: ServerMessage) => void;

/**
 * ConnectConnection — the page's one connection: status transitions, an
 * open handler, a message handler, a pending queue flushed on open,
 * exponential reconnect backoff, and half-open detection keyed on the
 * server's keepalive frames (the frame deadline). The send path
 * translates ClientMessages onto ChatService RPCs (grpc-send.ts).
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
  /** Whether the Subscribe stream is attached (the `ready` handshake
   * landed). `abort` alone cannot serve as this signal: it stays non-null
   * for the whole disconnect window (between a stream end and the next
   * connect attempt — seconds on the backoff schedule, indefinitely on
   * 'failed'), during which sends would hit the wire, fail, and surface as
   * spurious error bubbles — while the composer banner promises they will
   * be queued and sent on reconnect. */
  private streamAttached = false;
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
    // The old stream (if any) is torn down below — the attached flag dies
    // with it, so sends during this (re)connect window queue instead of
    // racing the not-yet-open stream onto the wire.
    this.streamAttached = false;
    this.teardownStream();
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }

    const abort = new AbortController();
    this.abort = abort;
    // Identity continuity: the stored token rides the open — the
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
    this.streamAttached = true;
    this.retryCount = 0;
    this.snapshotSeq = {}; // a fresh stream restarts the ordering slot-in
    if (el.kind.case !== 'ready') return; // guarded by the caller
    const ready = el.kind.value;
    storeSessionId(ready.sessionId);
    // The ready element is translated into the session_resumed frame:
    // the authoritative identity + the leases restore the focus and pull
    // the chat list. Synthesizing the frame keeps the handler table the
    // ONE place that logic lives.
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

  /** Drain the pending queue in order (awaited per send). The attachment
   * guard stops the drain the moment the stream detaches mid-flush — each
   * failed send re-queues itself, so a bare length loop would never end. */
  private async flushPending(): Promise<void> {
    while (this.pendingQueue.length > 0 && this.streamAttached && !this.disposed) {
      const d = this.pendingQueue.shift()!;
      await this.sendNow(d);
    }
  }

  private handleDisconnect(reason: string): void {
    if (this.disposed) return;
    this.connecting = false;
    this.streamAttached = false;
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
    if (!this.streamAttached || this.connecting) {
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
    await sendClientMessage(data, {
      onMessage: (msg) => this.onMessage?.(msg),
      isAttached: () => this.streamAttached,
      isDisposed: () => this.disposed,
      requeue: (msg) => this.pendingQueue.push(msg),
      reconnect: () => this.reconnect(),
    });
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
    this.streamAttached = false;
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
