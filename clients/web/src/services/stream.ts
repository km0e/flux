/**
 * stream.ts — the stream path's facade: the controller registry entry
 * point over the split modules.
 *
 * Provides: StreamController, getController, disposeController (+ the
 *           reasoning registry accessors and test helpers)
 * Depends: services/stream-state.ts (the shared registries),
 *          services/stream-controller.ts (the class body)
 *
 * The original single module split along its one honest seam — the shared
 * REGISTRIES (controllers + live reasoning blocks) vs the controller BODY
 * (the class). `stream-state.ts` holds the maps with no runtime dependency
 * on the class (breaking what would otherwise be an import cycle: the
 * class's idle check reads the registry, the registry constructs the
 * class); this facade is the only module that constructs. Everything the
 * module exported before is re-exported here — consumers and tests are
 * unchanged.
 *
 * The render discipline the class implements (P0-1's one-frame decoupling,
 * P0-2's append-only committed paragraphs) is documented on the class:
 * services/stream-controller.ts.
 */

import { StreamController } from './stream-controller';
import { controllers } from './stream-state';

export { StreamController } from './stream-controller';
export {
  disposeController,
  getReasoningEntry,
  _setReasoningEntryForTest,
  _resetReasoningForTest,
} from './stream-state';
export type { ReasoningState } from './stream-state';

export function getController(chatId: string): StreamController {
  let ctrl = controllers.get(chatId);
  if (!ctrl) {
    ctrl = new StreamController(chatId);
    controllers.set(chatId, ctrl);
  }
  return ctrl;
}
