/**
 * stream-state.ts — the stream path's shared registries.
 *
 * Split out of stream.ts: the maps BOTH the facade (getController
 * constructs controllers on demand) and the controller class (the idle
 * check reads the registry, the reasoning path reads/writes the live
 * reasoning blocks) need. Keeping them here breaks what would otherwise
 * be an import cycle — this module has NO runtime dependency on the
 * controller class (disposeController only needs its instance type).
 *
 * Provides: ReasoningState, controllers, reasoningBlocks, disposeController,
 *           getReasoningEntry, _setReasoningEntryForTest, _resetReasoningForTest
 * Depends: services/stream-controller.ts (TYPE ONLY)
 */

import type { StreamController } from './stream-controller';

/** A live reasoning block's DOM handles (registry entry; the type moved
 * here from core/types — it is stream-path plumbing, not app state). */
export interface ReasoningState {
  el: HTMLDetailsElement;
  content: HTMLDivElement;
}

export const controllers = new Map<string, StreamController>();

/** Live reasoning blocks keyed by chat. DOM references live HERE, not in
 * the zustand store — the store is app state, not an element registry, and
 * every consumer of a reasoning block is in the stream path's render. */
export const reasoningBlocks = new Map<string, ReasoningState>();

export function disposeController(chatId: string): void {
  const ctrl = controllers.get(chatId);
  if (ctrl) {
    ctrl.dispose();
    controllers.delete(chatId);
  }
}

/** Accessor for tests (the registry is module-private otherwise). */
export function getReasoningEntry(chatId: string): ReasoningState | undefined {
  return reasoningBlocks.get(chatId);
}

/** Test-only registry seeding + reset (mirrors _resetPanesForTest). */
export function _setReasoningEntryForTest(chatId: string, entry: ReasoningState): void {
  reasoningBlocks.set(chatId, entry);
}

export function _resetReasoningForTest(): void {
  reasoningBlocks.clear();
}
