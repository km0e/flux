/**
 * UsageStats.tsx — Token usage for the active chat, in a compact footer language:
 * `↑ input  ↓ output  R cache-read  W cache-write  ≈ $cost`.
 *
 * Four token stat units (not one pill): the icon sits in muted, the VALUE
 * in full contrast; the exact breakdown rides the Radix tooltip. The
 * fifth unit — the running cost estimate — rides only when the chat's
 * pinned model carries a models.dev price snapshot (`meta.cost`); it is
 * labeled ≈ because the estimate assumes the snapshot's rates for the
 * whole conversation.
 *
 * Provides: UsageStats, fmtTokens, fmtCost, estimateCost
 * Depends: core/state (UsageTotals), services/models (findSavedModel),
 *          components/ui/tooltip
 */
import { Tooltip } from './ui/tooltip';
import type { UsageTotals } from '../core/state';
import { findSavedModel } from '../services/models';

/** Compact token formatting: 999 → "999", 12_345 → "12.3k", 1_500_000 → "1.5m". */
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

/** Model price snapshot (USD per million tokens) — from the saved model's
 * models.dev meta, when the chat's pin has one. */
export interface ModelCost {
  input?: number;
  output?: number;
  cache_read?: number;
}

/** The running cost estimate over cumulative totals. Uncached prompt
 * (in − cached) bills at the input rate, cache reads at the cache-read
 * rate (fallback: input), output at the output rate. */
export function estimateCost(usage: UsageTotals, cost: ModelCost): number {
  const M = 1_000_000;
  const input = cost.input ?? 0;
  const uncachedPrompt = Math.max(0, usage.inTokens - usage.cachedTokens);
  return (
    (uncachedPrompt / M) * input +
    (usage.cachedTokens / M) * (cost.cache_read ?? input) +
    (usage.outTokens / M) * (cost.output ?? 0)
  );
}

export function UsageStats({
  usage,
  provider,
  model,
}: {
  usage: UsageTotals | undefined;
  provider?: string;
  model?: string;
}): React.ReactElement | null {
  if (!usage || (usage.inTokens === 0 && usage.outTokens === 0)) return null;
  // Cache write ≈ the uncached share of the prompt: OpenAI-compatible APIs
  // report cached_tokens inside prompt_tokens, so W = in − R per round and
  // the same subtraction holds for the sums.
  const write = Math.max(0, usage.inTokens - usage.cachedTokens);
  const exact = (n: number): string => n.toLocaleString();
  // Cost estimate rides only when the pinned model carries a price
  // snapshot — a missing price is never guessed.
  const price =
    provider && model
      ? (findSavedModel(provider, model)?.meta.cost as ModelCost | undefined)
      : undefined;
  const cost = price ? estimateCost(usage, price) : undefined;
  const priceNote = price
    ? ` · rates: $${price.input ?? 0}/M in, $${price.output ?? 0}/M out` +
      (price.cache_read !== undefined ? `, $${price.cache_read}/M cache` : '')
    : '';
  const title =
    `Input ${exact(usage.inTokens)} · Output ${exact(usage.outTokens)}` +
    ` · Cache read ${exact(usage.cachedTokens)} · Cache write ${exact(write)}` +
    ` · Context ${exact(usage.contextTokens)}` +
    (cost !== undefined ? ` · Est. ${fmtCost(cost)}${priceNote}` : '');
  return (
    <Tooltip content={title} side="bottom" openDelay={150}>
      <div id="usage-stats" className="flex items-center gap-2.5 whitespace-nowrap" aria-label={title}>
        <span className="inline-flex items-baseline gap-0.5">
          <span className="text-2xs text-muted">↑</span>
          <span className="text-2xs tabular-nums text-fg">{fmtTokens(usage.inTokens)}</span>
        </span>
        <span className="inline-flex items-baseline gap-0.5">
          <span className="text-2xs text-muted">↓</span>
          <span className="text-2xs tabular-nums text-fg">{fmtTokens(usage.outTokens)}</span>
        </span>
        {usage.cachedTokens > 0 && (
          <span className="inline-flex items-baseline gap-0.5">
            <span className="text-2xs text-info">R</span>
            <span className="text-2xs tabular-nums text-fg">{fmtTokens(usage.cachedTokens)}</span>
          </span>
        )}
        {write > 0 && (
          <span className="inline-flex items-baseline gap-0.5">
            <span className="text-2xs text-muted">W</span>
            <span className="text-2xs tabular-nums text-fg">{fmtTokens(write)}</span>
          </span>
        )}
        {cost !== undefined && (
          <span className="inline-flex items-baseline gap-0.5">
            <span className="text-2xs text-muted">≈</span>
            <span className="text-2xs tabular-nums text-fg">{fmtCost(cost)}</span>
          </span>
        )}
      </div>
    </Tooltip>
  );
}
