/**
 * id.test.ts — the client-side id helper.
 *
 * crypto.randomUUID is secure-context-only; newId must not depend on it
 * (plain-HTTP LAN deployments are a supported way to reach the server).
 *
 * Provides: newId behavior tests
 * Depends: ../lib/id
 */
import { describe, it, expect } from 'vitest';
import { newId } from '../../lib/id';

describe('newId', () => {
  it('produces UUID-v4-shaped ids', () => {
    const id = newId();
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
  });

  it('is unique across calls', () => {
    const ids = new Set(Array.from({ length: 1000 }, () => newId()));
    expect(ids.size).toBe(1000);
  });

  it('works when crypto.randomUUID is missing (non-secure context)', () => {
    const original = crypto.randomUUID;
    // Simulate a plain-HTTP origin: the API simply does not exist.
    Object.defineProperty(crypto, 'randomUUID', { value: undefined, configurable: true });
    try {
      expect(crypto.randomUUID).toBeUndefined();
      expect(newId()).toMatch(
        /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/,
      );
    } finally {
      Object.defineProperty(crypto, 'randomUUID', { value: original, configurable: true });
    }
  });
});
