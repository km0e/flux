/**
 * grpc-send.ts — the send-path translation: ClientMessage vocabulary onto
 * the ChatService RPCs.
 *
 * Split out of grpc-connection.ts (the connection's sendNow delegates
 * here). The switch is the whole translation — per-RPC timeouts, the
 * chat idempotency-key retry, and the lease-gate status → error-frame
 * synthesis (the handler table never learns the transport). The
 * connection's own behavior on failure (re-queue on a detached stream,
 * re-anchor the identity on UNAUTHENTICATED) is injected via SendContext,
 * so this module stays a pure translation with callbacks.
 *
 * Provides: sendClientMessage
 * Depends: core/grpc.ts, core/session.ts, core/types.ts, core/grpc-frames.ts, logger.ts, lib/id.ts
 */

import { clients } from './grpc';
import { readStoredSessionId } from './session';
import type { ClientMessage, ServerMessage } from './types';
import { log } from '../logger';
import { newId } from '../lib/id';
import { protoChatToChat } from './grpc-frames';

/** The connection-owned behavior the send path may invoke. */
export interface SendContext {
  /** Deliver a synthesized frame (ack or error) to the handler table. */
  onMessage: (msg: ServerMessage) => void;
  /** Whether the Subscribe stream is currently attached. */
  isAttached: () => boolean;
  /** Whether the connection is disposed (fail silent). */
  isDisposed: () => boolean;
  /** Park a message for the reconnect flush. */
  requeue: (msg: ClientMessage) => void;
  /** Tear down and re-open the stream (identity re-anchor). */
  reconnect: () => void;
}

/**
 * Translate and send one ClientMessage. Throws nothing outward for the
 * mapped transport statuses — they become error frames; anything the
 * connection must DO on failure rides the context.
 */
export async function sendClientMessage(data: ClientMessage, ctx: SendContext): Promise<void> {
  const token = readStoredSessionId() ?? undefined;
  const headers: Record<string, string> = token ? { 'x-flux-session': token } : {};
  try {
    switch (data.type) {
      case 'chat_list': {
        const resp = await clients.chat.listChats({}, { timeoutMs: 8000, headers });
        ctx.onMessage({ type: 'chats', chats: resp.chats.map(protoChatToChat) });
        return;
      }
      case 'chat_create': {
        const resp = await clients.chat.createChat(
          {
            name: data.name,
            workdir: data.workdir,
            provider: data.provider,
            model: data.model,
          },
          { timeoutMs: 8000, headers },
        );
        if (resp.error) {
          ctx.onMessage({ type: 'error', code: 'invalid_request', message: resp.error });
        } else if (resp.chat) {
          // The ack IS the chat_created payload (the broadcast rides the
          // stream as the fresh chats list).
          ctx.onMessage({ type: 'chat_created', chat: protoChatToChat(resp.chat) });
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
        // the caller — lease.ts.) A claim always
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
          ctx.onMessage({
            type: 'error',
            chat_id: data.chat_id,
            code: 'invalid_request',
            message: resp.error,
          });
        }
        return;
      }
      case 'fork': {
        // proto int64 → bigint; message row ids sit far below 2^53.
        const resp = await clients.chat.forkChat(
          { chatId: data.chat_id, forkPoint: BigInt(data.fork_point) },
          { timeoutMs: 8000, headers },
        );
        if (resp.error || !resp.chat) {
          // Validation failures (unknown chat, fork point not a user
          // message) ride the inline error (D4').
          ctx.onMessage({
            type: 'error',
            chat_id: data.chat_id,
            code: 'invalid_request',
            message: resp.error ?? 'fork failed',
          });
          return;
        }
        // The ack IS the chat_created payload — addChat auto-selects the
        // fork, and the claim's snapshot delivers the copied transcript.
        ctx.onMessage({ type: 'chat_created', chat: protoChatToChat(resp.chat) });
        return;
      }
      case 'question_response': {
        // Stale/unknown answers drop silently server-side — a refusal
        // surfaces no error.
        await clients.chat.answerQuestion(
          { chatId: data.chat_id, id: data.id, answer: data.answer },
          { timeoutMs: 8000, headers },
        );
        return;
      }
      default:
        // Management/fs families ride their own typed clients (services/
        // *.ts call core/grpc.ts directly) — nothing to translate here.
        log.debug('send ignored on the connect plane: ' + data.type);
        return;
    }
  } catch (err) {
    if (ctx.isDisposed()) return;
    const status = err as { code?: number; message?: string; rawMessage?: string };
    const chatId = 'chat_id' in data ? (data as { chat_id: string }).chat_id : undefined;
    // Lease-gate refusals → the error frames the handler table knows.
    if (status.code === 9) {
      ctx.onMessage({
        type: 'error',
        chat_id: chatId,
        code: 'chat_busy',
        message: 'Chat is in use by another session',
      });
      return;
    }
    if (status.code === 5) {
      ctx.onMessage({
        type: 'error',
        chat_id: chatId,
        code: 'chat_not_found',
        message: 'Chat not found' + (chatId ? `: ${chatId}` : ''),
      });
      return;
    }
    // 3 = INVALID_ARGUMENT — request-scoped validation, not a crash.
    if (status.code === 3) {
      ctx.onMessage({
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
      ctx.reconnect();
      return;
    }
    // The stream detached while this send was in flight (an idle-window
    // send that lost the race with a stream end). Definitive statuses
    // were already mapped above; a bare transport failure carries no
    // server verdict, so re-queue instead of surfacing an error bubble —
    // the reconnect flush delivers it (the same contract the banner
    // states for messages typed during the outage).
    if (!ctx.isAttached() && !ctx.isDisposed()) {
      log.info('send re-queued (stream detached mid-flight): ' + data.type);
      ctx.requeue(data);
      return;
    }
    // Transport-level failure with the stream still "attached" — the
    // next element (or deadline) will detach us; surface as an error.
    ctx.onMessage({
      type: 'error',
      chat_id: chatId,
      code: 'internal',
      message: status.message ?? 'request failed',
    });
  }
}
