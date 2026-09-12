/**
 * new-chat.ts — the shared new-chat flow.
 *
 * One dialog (components/dialogs): kind selection + workdir browsing resolve
 * together, then chat_create goes out. Used by the sidebar button and the
 * pane empty state.
 *
 * Provides: startNewChatFlow
 * Depends: services/dialogs.ts, core/bridge.ts, logger.ts
 */
import { dialogs } from './dialogs';
import { bridge } from '../core/bridge';
import { log } from '../logger';

export function startNewChatFlow(): void {
  log.info('new chat (kind + workdir picker)');
  void (async () => {
    const choice = await dialogs.pickNewChat();
    if (!choice) return; // dismissed
    // The pin is REQUIRED: the dialog gates Create on an explicit
    // provider AND model (providers carry no default model).
    bridge.send({
      type: 'chat_create',
      name: choice.kind === 'feature' ? 'Feature Chat' : 'New Chat',
      workdir: choice.workdir,
      kind: choice.kind,
      provider: choice.provider,
      model: choice.model,
    });
  })();
}
