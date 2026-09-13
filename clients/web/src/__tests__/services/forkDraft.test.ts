/**
 * forkDraft.test.ts — the fork-composer handoff (click stash → ack pair →
 * composer consume). The transcript copy EXCLUDES the fork point, so the
 * redo turn's content must reach the fork's composer without riding the
 * wire: keyed by the source at click time, re-keyed by the forked chat id
 * at the chat_created ack, consumed once at composer mount.
 */
import { describe, it, expect } from 'vitest';
import { stashForkDraft, pairForkDraft, takePendingDraft } from '../../services/forkDraft';

describe('forkDraft', () => {
  it('click → ack → composer: the draft lands on the fork, exactly once', () => {
    stashForkDraft('src', 'redo me');
    // Before the ack nothing is consumable — the fork's id is unknown yet.
    expect(takePendingDraft('fork-1')).toBeUndefined();
    expect(pairForkDraft('fork-1', 'src')).toBe(true);
    expect(takePendingDraft('fork-1')).toBe('redo me');
    // Consumed once: the composer remount (chat switch away and back)
    // never re-fills.
    expect(takePendingDraft('fork-1')).toBeUndefined();
  });

  it('an unpaired ack starts the composer empty (honest)', () => {
    // A fork created by another client (or a stale ack): no stash exists.
    expect(pairForkDraft('fork-2', 'no-such-source')).toBe(false);
    expect(takePendingDraft('fork-2')).toBeUndefined();
  });

  it('rapid forks from one source pair FIFO — acks race, order holds', () => {
    stashForkDraft('src', 'first');
    stashForkDraft('src', 'second');
    expect(pairForkDraft('fork-a', 'src')).toBe(true);
    expect(pairForkDraft('fork-b', 'src')).toBe(true);
    expect(takePendingDraft('fork-a')).toBe('first');
    expect(takePendingDraft('fork-b')).toBe('second');
  });

  it('different sources never cross-pair', () => {
    stashForkDraft('src-a', 'from a');
    stashForkDraft('src-b', 'from b');
    expect(pairForkDraft('fork-x', 'src-b')).toBe(true);
    expect(takePendingDraft('fork-x')).toBe('from b');
  });
});
