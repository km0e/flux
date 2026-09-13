/**
 * forkDraft.ts — The fork-composer handoff.
 *
 * A fork copies the source transcript UP TO (excluding) the fork point:
 * the redo turn re-enters the fork only when the user re-sends it, so
 * the fork's composer opens prefilled with the fork-point message's
 * content. The handoff is pure client-side sugar — the wire carries
 * nothing extra, and a fork whose user never sends is just a branch
 * paused before its first turn.
 *
 * Three moments, three functions:
 *   1. CLICK (`stashForkDraft`) — the fork affordance knows the content
 *      but not the fork's id (the server mints it), so the draft waits
 *      keyed by the SOURCE chat.
 *   2. ACK (`pairForkDraft`) — the `chat_created` handler correlates by
 *      `forked_from_chat_id` and attaches the oldest stashed draft to
 *      the new chat id.
 *   3. MOUNT (`takePendingDraft`) — the fork's composer consumes its
 *      draft once (ChatInput remounts per chat switch, so the consume
 *      lands exactly on the fork's first composer).
 *
 * Provides: stashForkDraft, pairForkDraft, takePendingDraft
 */

/** Per-source FIFO: fork clicks push, forked `chat_created` acks shift —
 * acks race, so a queue (not a slot) keeps rapid forks from the same
 * source paired. An unpaired stash (the RPC failed) lingers harmlessly
 * until the next fork from that source. */
const stashed = new Map<string, string[]>();

/** Drafts attached to a forked chat id, consumed once by its composer. */
const paired = new Map<string, string>();

/** Click time: the fork point's content waits on its source chat id. */
export function stashForkDraft(sourceChatId: string, content: string): void {
  const queue = stashed.get(sourceChatId) ?? [];
  queue.push(content);
  stashed.set(sourceChatId, queue);
}

/** Ack time: pair the forked chat with the oldest stashed draft from its
 * source. Returns false when no stash exists (a fork created by another
 * client, or a stale ack) — the composer then starts empty, which is
 * honest: the draft never existed on this client. */
export function pairForkDraft(forkedChatId: string, sourceChatId: string): boolean {
  const queue = stashed.get(sourceChatId);
  const text = queue?.shift();
  if (queue && queue.length === 0) stashed.delete(sourceChatId);
  if (text === undefined) return false;
  paired.set(forkedChatId, text);
  return true;
}

/** Mount time: the fork's composer consumes its draft (once). */
export function takePendingDraft(chatId: string): string | undefined {
  const text = paired.get(chatId);
  if (text !== undefined) paired.delete(chatId);
  return text;
}
