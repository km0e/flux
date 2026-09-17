/**
 * grpc-frames.ts — the PURE translation layer between the flux.v1 proto
 * vocabulary and the wire frames the handler table dispatches.
 *
 * Split out of grpc-connection.ts (which re-exports these for stability):
 * the stream's SubscribeResponse elements translate ONTO the ServerMessage
 * shapes (elementToFrame), the R2 sequence reconciliation gates
 * pre-snapshot replays (reconcileElement), and the send path's acks map
 * back through protoChatToChat. No state, no timers — everything here is
 * a pure function of its arguments, which is what the unit tests exercise
 * directly.
 *
 * Provides: protoChatToChat, protoMessageToHistory, elementToFrame,
 *           reconcileElement
 * Depends: core/grpc.ts (mcp kind/state mappers), core/types.ts
 */

import { mcpKindOf, mcpStateOf } from './grpc';
import type {
  ChatInfo,
  HistoryMessage,
  SavedModelInfo,
  ServerMessage,
} from './types';
import type { ChatInfo as ProtoChatInfo, Message as ProtoMessage } from '../gen/flux/v1/common_pb';
import type { SubscribeResponse } from '../gen/flux/v1/events_pb';
import { ChatStateKind as ProtoChatStateKind } from '../gen/flux/v1/common_pb';
import { SkillSource as ProtoSkillSource } from '../gen/flux/v1/common_pb';

// ── proto → wire mappers ───────────────────────────────────────────────

/** proto ChatInfo → the wire ChatInfo the store/handlers consume. */
export function protoChatToChat(c: ProtoChatInfo): ChatInfo {
  return {
    chat_id: c.chatId,
    name: c.name,
    created_at: c.createdAt,
    last_activity_at: c.lastActivityAt,
    active: c.active,
    running: c.running,
    workdir: c.workdir,
    provider: c.provider,
    model: c.model,
    forked_from_chat_id: c.forkedFromChatId ?? undefined,
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
    case 'chatHistory': {
      return {
        type: 'chat_history',
        chat_id: chatId,
        messages: k.value.messages.map(protoMessageToHistory),
      };
    }
    case 'messagePersisted': {
      const id = k.value.id;
      return {
        type: 'message_persisted',
        chat_id: chatId,
        id: typeof id === 'bigint' ? Number(id) : 0,
        content: k.value.content,
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
      // No non-null assertion: a chatCreated without a payload is a
      // malformed element — throw so the stream loop logs and skips it
      // (the element carries nothing dispatchable).
      if (!k.value.chat) throw new Error('chatCreated without a chat payload');
      return { type: 'chat_created', chat: protoChatToChat(k.value.chat) };
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
          kind: mcpKindOf(s.kind),
          command: s.command,
          args: s.args,
          env_keys: s.envKeys,
          url: s.url,
          header_keys: s.headerKeys,
          state: mcpStateOf(s.state),
          tool_names: s.toolNames,
        })),
      };
    case 'mcpNotice':
      return {
        type: 'mcp_notice',
        server_id: k.value.serverId,
        level: k.value.level,
        message: k.value.message,
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
