/**
 * id.ts — client-side unique ids.
 *
 * `crypto.randomUUID` exists ONLY in secure contexts (HTTPS or localhost) —
 * the server is routinely reached over plain HTTP from a LAN address
 * (`--host 0.0.0.0` + no TLS), where the API is undefined and every call
 * throws. Build the id from `crypto.getRandomValues` instead: available in
 * every context, no usage change needed at call sites.
 *
 * Provides: newId
 */

/** A UUID-v4-shaped random id (collision-safe for UI-scope ids: tab keys,
 * DOM hooks, per-session handles — the server's durable ids are the uuid
 * crate's, not this). */
export function newId(): string {
  // 16 random bytes → the 8-4-4-4-12 layout with v4 bits set.
  const bytes = new Uint8Array(16);
  crypto.getRandomValues(bytes);
  bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
  bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
  const hex = [...bytes].map((b) => b.toString(16).padStart(2, '0'));
  return `${hex.slice(0, 4).join('')}-${hex.slice(4, 6).join('')}-${hex.slice(6, 8).join('')}-${hex
    .slice(8, 10)
    .join('')}-${hex.slice(10, 16).join('')}`;
}
