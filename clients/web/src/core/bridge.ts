/**
 * bridge.ts — typed message bus from UI code to the active transport
 * (the WS connection).
 *
 * Replaces the ad-hoc `window.__flux*` global assignments with a single
 * injectable module-level singleton. Tests call `resetBridgeForTest()` to
 * isolate, then `setBridge()` with mock handlers.
 *
 * Provides: bridge, HostBridge, setBridge, resetBridgeForTest
 * Depends: core/types.ts
 */
import type { ClientMessage } from './types';

export interface HostBridge {
  send(msg: ClientMessage): void;
  reconnect(): void;
}

const noop = () => {};

let current: HostBridge = {
  send: noop,
  reconnect: noop,
};

/** Inject bridge handlers (typically called from index.tsx at init). */
export function setBridge(partial: Partial<HostBridge>): void {
  if (partial.send) current.send = partial.send;
  if (partial.reconnect) current.reconnect = partial.reconnect;
}

/** Reset bridge to no-ops (for test isolation). */
export function resetBridgeForTest(): void {
  current = { send: noop, reconnect: noop };
}

export const bridge: HostBridge = {
  get send() {
    return current.send;
  },
  get reconnect() {
    return current.reconnect;
  },
};
