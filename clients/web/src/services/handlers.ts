/**
 * handlers.ts — Table-driven WebSocket message handler registry.
 *
 * One table: the WebSocket wire (ServerMessage, registered via
 * registerAllHandlers). Host-embedding concerns (dialogs, question
 * walk-throughs, workspace) live behind the ChatHost capability interface
 * (core/host.ts) — the embedding implements it, this file only calls it.
 *
 * Provides: registerAllHandlers
 * Depends: services/dispatch.ts, services/stream-handler.ts, services/history.ts,
 *          services/panes.ts, logger.ts
 */

import { registerHandler, handleServerError, type MessageHandler, type DispatchContext } from './dispatch';
import {
  handleStreamError,
  handleStreamGap,
  handleStreamEnd,
  handleStreamCancelled,
  handleTextDelta,
  handleReasoningDelta,
  handleToolStart,
  handleToolPreview,
  handleToolResult,
  discardInterrupt,
} from './stream-handler';
import { renderHistoryMessages } from './history';
import { clearChatPane, getPaneIfExists, getPaneIds } from './panes';
import { handleModelsFrame } from './models';
import { handleSkillsMessage } from './skills';
import { pruneSessions } from './terminal';
import { createNoticeBubble, scrollPaneToBottom } from '../lib/dom';
import { storeSessionId } from '../core/session';
import { log } from '../logger';
import { dialogs } from './dialogs';
import { useFlux } from '../core/state';
import type { ChatInfo, ServerMessage } from '../core/types';
import type { Chat } from '../core/state';

/** Map a wire `ChatInfo` to the app's `Chat` shape. */
function toChat(c: ChatInfo): Chat {
  return {
    id: c.chat_id,
    name: c.name,
    createdAt: new Date(c.created_at).getTime(),
    lastActivityAt: new Date(c.last_activity_at).getTime(),
    active: c.active,
    kind: c.kind,
    workdir: c.workdir,
    provider: c.provider,
    model: c.model,
  };
}

// `satisfies` pins every key to a valid `ServerMessage['type']` — a typo'd key
// becomes a compile error. Note: `satisfies` sits on the initializer, not the
// type annotation, because esbuild (media build) does not parse `satisfies`
// after a declaration annotation. The satisfies target still contextually
// types the handler params, so the bodies keep their inferred signatures.
// Mapped-type handler table: each entry's `msg` parameter is the message
// narrowed to its own key (no per-handler `if (msg.type !== ...) return`
// guards) — dispatchMessage looks handlers up BY `msg.type`, so the
// pairing the types assume is the one the dispatcher performs.
type WireHandler<K extends ServerMessage['type']> = (
  msg: Extract<ServerMessage, { type: K }>,
  ctx: DispatchContext,
) => void;

type HandlerTable = { [K in ServerMessage['type']]?: WireHandler<K> };

const HANDLERS = {
  session_resumed: (msg, ctx) => {
    // The authoritative identity — adopted or freshly minted; always
    // overwrite the stored one.
    storeSessionId(msg.session_id);
    // Restore focus: a persisted active chat the identity still holds wins;
    // otherwise fall back to the first leased chat. setState — a direct field
    // write would bypass the mount.ts subscription that drives the lease
    // handover (switchLease + pane materialization).
    if (!useFlux.getState().activeChatId && msg.leases.length > 0) {
      useFlux.setState({ activeChatId: msg.leases[0] });
    }
    ctx.conn.send({ type: 'chat_list' });
  },

  chats: (msg, ctx) => {
    ctx.state.setChats(msg.chats.map((c) => toChat(c)));
    // The list is authoritative — a pane whose chat is missing was deleted
    // outside the confirm flow (e.g. by another panel). Drop it so stale
    // DOM and a live controller do not linger for a deleted conversation.
    const live = new Set(msg.chats.map((c) => c.chat_id));
    for (const id of getPaneIds()) {
      if (!live.has(id)) clearChatPane(id);
    }
    // Terminal sessions of deleted chats die too (the server reaps their
    // PTYs; this drops the dead sockets + tab metas).
    pruneSessions(live);
    // Wire `active` = lease held; a freed lease (active: false) means the
    // read-only mark from a previous chat_busy is stale — the viewer may
    // now claim, so clear it. Re-taking the lease stays EXPLICIT: the
    // viewer pane offers a Take over button instead of auto-claiming.
    for (const c of msg.chats) {
      if (!c.active) ctx.state.setReadOnly(c.chat_id, false);
    }
    // D-06: a programmatic re-select (activeChatId survives reconnects,
    // loadedChatId was cleared by resetStreamingForReconnect; the delete
    // fallback goes through switchLease) re-opens via claim — the single
    // message carries history snapshot + subscription + lease. An
    // already-loaded chat is skipped (avoids a full live re-render); the
    // server treats repeated claims idempotently.
    const active = ctx.state.activeChatId;
    if (active && ctx.state.loadedChatId !== active) {
      ctx.conn.send({ type: 'chat_claim', chat_id: active });
    }
  },

  chat_created: (msg, ctx) => {
    ctx.state.addChat(toChat(msg.chat));
  },

  /** Provider registry broadcast — the pickers' data source. */
  providers: (msg, ctx) => {
    ctx.state.set({ providers: msg.providers });
  },

  /** LOCAL saved-model registry broadcast — the pickers' and the
   * Providers dialog's Models-section data source (authoritative list
   * replaces the store slice). */
  models: (msg, _ctx) => {
    handleModelsFrame(msg.models);
  },

  /** MCP launch-list broadcast — the MCP dialog's data source (also the
   * post-mutation fresh list). */
  mcp_servers: (msg, ctx) => {
    ctx.state.set({ mcpServers: msg.servers });
  },

  /** Skills broadcast — the Skills dialog's data source. */
  skills: (msg, _ctx) => {
    handleSkillsMessage(msg.skills);
  },

  /** The provider hot-swap landed: update the chat identity, drop a
   * neutral notice in the pane (the conversation continues seamlessly). */
  provider_switched: (msg, ctx) => {
    ctx.state.updateChatProvider(msg.chat_id, msg.provider, msg.model);
    const pane = getPaneIfExists(msg.chat_id);
    if (pane) {
      pane.appendChild(createNoticeBubble(`⇄ Provider switched to ${msg.provider} · ${msg.model}`));
      scrollPaneToBottom(pane, 50);
    }
    log.info(`provider_switched [${msg.chat_id}] ${msg.provider}/${msg.model}`);
  },

  chat_history: (msg, ctx) => {
    ctx.state.set({ loadedChatId: msg.chat_id });
    // Async: waits for the deferred highlighter chunk before painting
    // (see renderHistoryMessages) — loadedChatId is set synchronously above.
    void renderHistoryMessages(msg.chat_id, msg.messages);
  },

  /** D-07: the authoritative round-state snapshot (rides the open/claim
   * response) — the client converges its streaming state from this instead
   * of inferring from events: reconnects / gap reloads / multi-window no
   * longer drift. The snapshot is authoritative, so any in-flight R1
   * interrupt-sequence bookkeeping retires to it. idle = the server
   * confirms the round ended → clear streaming. */
  chat_state: (msg, ctx) => {
    discardInterrupt(msg.chat_id);
    if (msg.state === 'idle') {
      ctx.state.clearStreaming(msg.chat_id);
      dismissQuestionWait(msg.chat_id);
    } else {
      // streaming: a round is in flight (a question wait counts as a tool
      // flight) — the wait hint, if present, is stale
      ctx.state.setStreaming(msg.chat_id, true);
      dismissQuestionWait(msg.chat_id);
    }
  },

  /** Feature mode: the context re-scaffolded at the feature boundary (rebase
   * archive) — insert a neutral notice into the chat flow (the history stays
   * viewable in the DB but is no longer in the model's context). */
  context_rebased: (msg, _ctx) => {
    const pane = getPaneIfExists(msg.chat_id);
    if (!pane) return;
    pane.appendChild(createNoticeBubble(`⟳ Context archived from message #${msg.base_message_id}`));
    scrollPaneToBottom(pane, 50);
    log.info('context_rebased [' + msg.chat_id + '] base=' + msg.base_message_id);
  },

  /**
   * Unified error channel (D-12): chat-scoped errors dispatch by code —
   * chat_busy flips the pane to read-only watch, stream_gap offers a reload,
   * everything else is a stream error; chat-less errors bubble globally.
   */
  error: (msg, ctx) => {
    log.error('error [' + (msg.chat_id ?? 'server') + '] ' + msg.code + ': ' + msg.message);
    if (msg.chat_id === undefined) {
      handleServerError(msg.message);
      return;
    }
    switch (msg.code) {
      case 'chat_busy': {
        // Two arrivals: (a) an operation was refused because another session
        // holds the lease — watch read-only; (b) a DEMOTION: another session
        // claimed a chat WE held (the claim steals the lease — ops.rs
        // claim_chat) and the server tells the old holder with this same
        // frame. Either way the pane goes read-only with an explicit Take
        // over button (ChatView reads the readonly flag reactively) — and
        // the takeover itself now lands: claiming a live holder's chat
        // steals the lease. The chat_open (re)establishes the subscription
        // and pulls history; the server skips the resend idempotently if
        // already subscribed.
        ctx.state.setReadOnly(msg.chat_id, true);
        ctx.conn.send({ type: 'chat_open', chat_id: msg.chat_id });
        break;
      }
      case 'stream_gap':
        dismissQuestionWait(msg.chat_id);
        handleStreamGap(msg.chat_id);
        break;
      default:
        dismissQuestionWait(msg.chat_id);
        handleStreamError(msg.chat_id, msg.message);
    }
  },

  stream_cancelled: (msg) => {
    log.info('stream cancelled [' + msg.chat_id + ']');
    dismissQuestionWait(msg.chat_id);
    handleStreamCancelled(msg.chat_id);
  },

  question_required: (msg, ctx) => {
    // The model's question tool awaits an answer: sync an in-pane context
    // hint and let the dialog layer collect the answer with its own UX (an
    // inline card in the conversation flow) — the question text and options
    // are entirely agent-produced. The answer goes back over question_response.
    showQuestionWait(msg.chat_id, msg.question.text);
    void dialogs
      .askQuestion(msg.chat_id, { text: msg.question.text, options: msg.question.options })
      .then((answer) => {
        dismissQuestionWait(msg.chat_id);
        ctx.conn.send({ type: 'question_response', chat_id: msg.chat_id, id: msg.id, answer });
      })
      .catch((err: unknown) => {
        log.error('question flow failed: ' + (err instanceof Error ? err.message : String(err)));
      });
  },

  usage: (msg, ctx) => {
    ctx.state.addUsage(msg.chat_id, {
      prompt_tokens: msg.prompt_tokens,
      completion_tokens: msg.completion_tokens,
      cached_tokens: msg.cached_tokens,
    });
  },

  stream_end: (msg) => {
    dismissQuestionWait(msg.chat_id);
    handleStreamEnd(msg.chat_id, msg.finish_reason);
  },

  text_delta: (msg) => {
    handleTextDelta(msg.chat_id, msg.delta);
  },

  reasoning_delta: (msg) => {
    handleReasoningDelta(msg.chat_id, msg.delta);
  },

  tool_start: (msg) => {
    handleToolStart(msg.chat_id, msg.id, msg.name, msg.arguments);
  },

  /** tool_preview — the model is still forming a tool call: identity card
   * + optional argument fragments, before the real tool_start. */
  tool_preview: (msg) => {
    handleToolPreview(msg.chat_id, msg.id, msg.name, msg.arguments_delta);
  },

  tool_result: (msg) => {
    // The question tool's tool_result (answered/dismissed) landed → retire the wait hint.
    dismissQuestionWait(msg.chat_id);
    handleToolResult(msg.chat_id, msg.id, msg.result);
  },

} satisfies HandlerTable;

/** Register every handler in HANDLERS. Call once at startup. */
export function registerAllHandlers(): void {
  for (const [type, handler] of Object.entries(HANDLERS)) {
    // The table's handlers are narrowed per key (they accept only their
    // own message type — contravariance makes the widening cast neces-
    // sary); dispatchMessage guarantees the pairing by looking handlers
    // up by msg.type, so the cast is sound.
    registerHandler(type, handler as MessageHandler);
  }
}

/**
 * In-pane wait hint for the model's question: the host pops its own picker,
 * the pane carries the question context. Idempotent insert; cleared by
 * dismiss on tool_result / round wrap-up.
 */
function showQuestionWait(chatId: string, question: string): void {
  const pane = getPaneIfExists(chatId);
  if (!pane || pane.querySelector('#question-wait')) return;
  const bar = createNoticeBubble(`Waiting for your answer: ${question}`);
  bar.id = 'question-wait';
  bar.classList.add('question-wait');
  pane.appendChild(bar);
  scrollPaneToBottom(pane, 50);
}

/** Remove the question wait hint (idempotent). */
function dismissQuestionWait(chatId: string): void {
  getPaneIfExists(chatId)?.querySelector('#question-wait')?.remove();
}

