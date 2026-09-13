/**
 * dispatch.ts — wire message dispatch via a handler registry.
 *
 * The Connect plane's translated frames (ServerMessage shapes, delivered
 * by core/grpc-connection.ts) dispatch here by message type. The
 * embedding-host channel (dialogs, question round-trips) does NOT flow
 * through here — it is owned by the dialog service (services/dialogs.ts).
 *
 * Provides: registerHandler, dispatchMessage, DispatchContext, handleServerError
 * Depends: core/state.ts, core/bridge.ts, core/types.ts,
 *          services/stream-handler.ts
 */

import { log } from '../logger';
import { useFlux, type FluxStore } from '../core/state';
import { bridge } from '../core/bridge';
import { createErrorBubble } from '../lib/dom';
import { handleStreamError } from './stream-handler';
import type { ServerMessage } from '../core/types';

/** The connection facet the handlers use: send (the transport translates
 * onto its wire). The Connect connection satisfies it. */
export interface ConnectionLike {
  send(msg: import('../core/types').ClientMessage): void;
}

export interface DispatchContext {
  /** The live zustand store. MUST be wired as a getter — zustand setState
   * replaces the state object, so a snapshot taken at mount would read
   * stale fields forever. Handlers may also call store actions off it
   * (actions are stable closures over the internal set). */
  readonly state: FluxStore;
  conn: ConnectionLike;
  bridge: typeof bridge;
}

export type MessageHandler = (msg: ServerMessage, ctx: DispatchContext) => void;

const handlers = new Map<string, MessageHandler>();

/** Register a handler for a server message type. Call at startup. */
export function registerHandler(type: string, handler: MessageHandler): void {
  handlers.set(type, handler);
}

/** Dispatch a server message to the registered handler for its type. */
export function dispatchMessage(msg: ServerMessage, ctx: DispatchContext): void {
  log.debug('dispatch: ' + msg.type);
  const handler = handlers.get(msg.type);
  if (handler) {
    handler(msg, ctx);
  }
}

export function handleServerError(message: string): void {
  const cid = useFlux.getState().activeChatId;
  if (cid) {
    handleStreamError(cid, message);
  } else {
    const wrap = document.getElementById('messages-wrap');
    if (wrap) {
      wrap.appendChild(createErrorBubble(message));
    }
  }
}
