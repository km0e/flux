/**
 * acks.ts — pending WS request/reply correlation (the shared plumbing
 * behind the promise-shaped service calls).
 *
 * Several wire round-trips carry no request id: the reply is correlated by
 * an echoed key (the requested path, the provider id, the model pair). Five
 * services used to hand-roll that correlation — with one real bug class: a
 * second concurrent caller for the same key OVERWROTE the first caller's
 * resolver, so the first promise hung out its whole timeout window and then
 * reported a false "no reply" although the reply had arrived. This helper
 * makes the sharing semantics explicit:
 *
 *   - the FIRST caller for a key sends the request (and arms its watchdog,
 *     if any); later concurrent callers JOIN the in-flight one — one wire
 *     request, one reply, every caller resolved;
 *   - `settle` resolves (and clears) everyone waiting on the key; a reply
 *     after a timeout/outage settle is a dropped no-op (the same
 *     stale-reply semantics the hand-rolled maps had);
 *   - `settleAll` resolves every pending key — the outage flush.
 *
 * `send` must not throw (the connection manager queues on a closed socket
 * instead of raising); a throwing send would leave the entry dangling.
 *
 * Provides: AckWait, DISCONNECTED_ERROR
 * Depends: (none — pure plumbing; each service wires its own store
 *           subscription for the disconnect flush)
 */

/** The outage marker — resolved instead of the timeout when the connection
 * is down (fail fast) or dropped mid-request (flush). Callers test it with
 * `isDisconnect` to suppress their own error toasts: outage communication
 * belongs to the connection indicator, not per-surface error popups. */
export const DISCONNECTED_ERROR = 'server disconnected';

export class AckWait<K, R> {
  private pending = new Map<K, Array<(result: R) => void>>();
  private watchdogs = new Map<K, ReturnType<typeof setTimeout>>();

  /**
   * Join the wait queue for `key`. The first caller runs `send()` (one
   * wire request per in-flight key); concurrent later callers share the
   * reply. With a `watchdog`, the request settles itself after `timeoutMs`
   * with `watchdog.onTimeout(key)` — an interactive surface's answer for
   * a server that never replies.
   */
  wait(
    key: K,
    send: () => void,
    watchdog?: { timeoutMs: number; onTimeout: (key: K) => R },
  ): Promise<R> {
    return new Promise<R>((resolve) => {
      const queued = this.pending.get(key);
      if (queued) {
        queued.push(resolve);
        return;
      }
      this.pending.set(key, [resolve]);
      if (watchdog) {
        const timer = setTimeout(() => this.settle(key, watchdog.onTimeout(key)), watchdog.timeoutMs);
        this.watchdogs.set(key, timer);
      }
      send();
    });
  }

  /** Resolve every waiter for `key` and drop the entry (idempotent). */
  settle(key: K, result: R): void {
    const watchdog = this.watchdogs.get(key);
    if (watchdog !== undefined) {
      clearTimeout(watchdog);
      this.watchdogs.delete(key);
    }
    const resolvers = this.pending.get(key);
    if (!resolvers) return; // already settled, or never waited
    this.pending.delete(key);
    for (const resolve of resolvers) resolve(result);
  }

  /** Resolve every pending key via `fallback(key)` — the outage flush. */
  settleAll(fallback: (key: K) => R): void {
    for (const key of [...this.pending.keys()]) {
      this.settle(key, fallback(key));
    }
  }
}
