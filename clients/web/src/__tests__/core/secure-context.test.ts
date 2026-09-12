/**
 * secure-context.test.ts — source-convention guard.
 *
 * `crypto.randomUUID` exists ONLY in secure contexts (HTTPS / localhost).
 * The server is routinely reached over plain HTTP from a LAN address, so
 * no module may call it directly — all client-side ids go through
 * `lib/id.ts`'s `newId()` (built on `crypto.getRandomValues`, which IS
 * available everywhere). The one direct call that slipped through broke
 * every send over plain HTTP.
 *
 * Scans every source module (via Vite's raw glob) for the literal API
 * outside lib/id.ts and fails the suite on sight. Comments are stripped
 * first (docs legitimately mention the API). No ESLint infra — a vitest
 * scan is the cheap pin.
 */
import { describe, it, expect } from 'vitest';

const sources = import.meta.glob<string>('/src/**/*.{ts,tsx}', {
  query: '?raw',
  import: 'default',
  eager: true,
});

/** Strip block comments, then line comments — enough to keep doc mentions
 * of the API from tripping the scan (a `//` inside a string literal can
 * only over-strip, never fabricate a match). */
function stripComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .map((line) => line.replace(/(^|\s)\/\/.*$/, '$1'))
    .join('\n');
}

describe('secure-context guard', () => {
  it('never calls crypto.randomUUID outside lib/id.ts', () => {
    const offenders = Object.entries(sources)
      .filter(([path]) => !path.endsWith('/lib/id.ts'))
      .filter(([path]) => !path.includes('/__tests__/') && !path.includes('/gen/'))
      .filter(([, src]) => stripComments(src).includes('crypto.randomUUID'))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });
});
