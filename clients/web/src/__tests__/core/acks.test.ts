/**
 * acks.test.ts — the shared WS request/reply correlation helper.
 *
 * Pins the semantics the five services ride: one wire request per
 * in-flight key, concurrent callers share the reply, a settle is
 * idempotent (late replies drop), the watchdog bounds the wait, and
 * settleAll flushes every pending key (the outage path).
 */
import { describe, it, expect, vi } from 'vitest';
import { AckWait } from '../../core/acks';

describe('AckWait', () => {
  it('first caller sends, later concurrent callers share the reply', async () => {
    const acks = new AckWait<string, string>();
    const send = vi.fn();
    const p1 = acks.wait('k', send);
    const p2 = acks.wait('k', send);
    const p3 = acks.wait('k', send);
    // Exactly ONE wire request for the shared key.
    expect(send).toHaveBeenCalledTimes(1);
    acks.settle('k', 'reply');
    await expect(p1).resolves.toBe('reply');
    await expect(p2).resolves.toBe('reply');
    await expect(p3).resolves.toBe('reply');
  });

  it('distinct keys send independently', () => {
    const acks = new AckWait<string, number>();
    const send = vi.fn();
    acks.wait('a', send);
    acks.wait('b', send);
    expect(send).toHaveBeenCalledTimes(2);
  });

  it('a late reply after a timeout settle is a dropped no-op', async () => {
    vi.useFakeTimers();
    try {
      const acks = new AckWait<string, string>();
      const p = acks.wait('k', () => {}, {
        timeoutMs: 1000,
        onTimeout: () => 'timed out',
      });
      vi.advanceTimersByTime(1000);
      await expect(p).resolves.toBe('timed out');
      // The reply arrives late — nothing hangs, nothing resolves again.
      expect(() => acks.settle('k', 'late')).not.toThrow();
      // And the key is free again: a NEW caller sends a fresh request.
      const send = vi.fn();
      const next = acks.wait('k', send);
      expect(send).toHaveBeenCalledTimes(1);
      acks.settle('k', 'fresh');
      await expect(next).resolves.toBe('fresh');
    } finally {
      vi.useRealTimers();
    }
  });

  it('the watchdog resolves every waiter sharing the key (shared fate)', async () => {
    vi.useFakeTimers();
    try {
      const acks = new AckWait<string, { error: string }>();
      const late = acks.wait('k', () => {}, {
        timeoutMs: 500,
        onTimeout: (key) => ({ error: `timeout of ${key}` }),
      });
      const joined = acks.wait('k', () => {});
      vi.advanceTimersByTime(500);
      await expect(late).resolves.toEqual({ error: 'timeout of k' });
      await expect(joined).resolves.toEqual({ error: 'timeout of k' });
    } finally {
      vi.useRealTimers();
    }
  });

  it('settleAll flushes every pending key through fallback(key)', async () => {
    const acks = new AckWait<string, string>();
    const a = acks.wait('a', () => {});
    const b = acks.wait('b', () => {});
    acks.settleAll((key) => `flush:${key}`);
    await expect(a).resolves.toBe('flush:a');
    await expect(b).resolves.toBe('flush:b');
  });

  it('settle clears the watchdog (no stray timer settling a new wait)', async () => {
    vi.useFakeTimers();
    try {
      const acks = new AckWait<string, string>();
      const p1 = acks.wait('k', () => {}, { timeoutMs: 100, onTimeout: () => 'late' });
      acks.settle('k', 'ok');
      await expect(p1).resolves.toBe('ok');
      // A fresh caller's full window must elapse — the first caller's
      // watchdog must not short-circuit it.
      const p2 = acks.wait('k', () => {}, { timeoutMs: 100, onTimeout: () => 'late' });
      vi.advanceTimersByTime(100);
      await expect(p2).resolves.toBe('late');
    } finally {
      vi.useRealTimers();
    }
  });
});
