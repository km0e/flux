/**
 * format.ts — human-readable byte formatting.
 *
 * "1.2 KB"-style sizes, shared by the Explorer rows and the file preview's
 * truncation hint (one language for byte counts across the UI).
 *
 * Provides: fmtBytes, fmtTokens
 */

export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** Token counts on picker hints: 128000 → "128k", 1048576 → "1M",
 * 1572864 → "1.6M". Below 1k renders bare. */
export function fmtTokens(n: number): string {
  if (n >= 1_000_000) {
    const m = n / 1_000_000;
    return `${Number.isInteger(m) ? m : m.toFixed(1)}M`;
  }
  if (n >= 1_000) {
    const k = n / 1_000;
    return `${Number.isInteger(k) ? k : Math.round(k)}k`;
  }
  return String(n);
}
