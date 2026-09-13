/**
 * format.ts — human-readable formatters shared across the UI (one home
 * for the compact number/time languages: byte sizes, token counts, USD
 * estimates, relative timestamps).
 *
 * Provides: fmtBytes, fmtTokens, fmtCost, relativeTime
 */

export function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

/** Compact token formatting: 999 → "999", 12_345 → "12.3k", 1_500_000 →
 * "1.5m". One decimal under 100 of a unit (stripped when .0), integer
 * above — the SAME language everywhere a token count is shown (usage
 * stats, picker hints, catalog rows). */
export function fmtTokens(n: number): string {
  if (n < 1000) return String(n);
  const unit = n < 1_000_000 ? 'k' : 'm';
  const v = n / (unit === 'k' ? 1000 : 1_000_000);
  const body = v >= 100 ? String(Math.round(v)) : v.toFixed(1).replace(/\.0$/, '');
  return body + unit;
}

/** Compact USD formatting: 0.5 → "$0.50", 1.234 → "$1.23", 12.5 → "$12.50",
 * 1250 → "$1.25k". */
export function fmtCost(usd: number): string {
  if (usd >= 1000) return `$${(usd / 1000).toFixed(2)}k`;
  return `$${usd.toFixed(2)}`;
}

/** Relative time for the chat list: "now", "5m", "3h", "2d", else a date. */
export function relativeTime(ts: number): string {
  const diff = Date.now() - ts;
  if (diff < 60_000) return 'now';
  if (diff < 3_600_000) return `${Math.floor(diff / 60_000)}m`;
  if (diff < 86_400_000) return `${Math.floor(diff / 3_600_000)}h`;
  if (diff < 14 * 86_400_000) return `${Math.floor(diff / 86_400_000)}d`;
  const d = new Date(ts);
  return `${d.getMonth() + 1}/${d.getDate()}`;
}
