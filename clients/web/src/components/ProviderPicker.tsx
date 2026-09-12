/**
 * ProviderPicker.tsx — provider select + model input, shared by the
 * new-chat dialog and the composer's switcher.
 *
 * The list is the truth: only real registry entries render (there is no
 * synthetic "server default" option — `chat_create` requires an explicit
 * provider pin). The model input carries a datalist fed from the store's
 * probed-catalog cache — populated by the Providers dialog (the ONE place
 * that probes). This picker never issues a probe: creating a chat must
 * not stall on upstream round-trips, and an uncached catalog degrades to
 * plain free-text (a model string is never gated on the probe).
 *
 * Provides: ProviderPicker
 * Depends: core/state.ts, services/providers.ts, components/ui/*
 */
import { useEffect } from 'react';
import { useFlux } from '../core/state';
import { fetchProviders } from '../services/providers';
import { fetchModels } from '../services/models';
import { fmtTokens } from '../lib/format';
import { cn } from '../lib/cn';

export function ProviderPicker(props: {
  providerId: string;
  model: string;
  onChange: (next: { provider: string; model: string }) => void;
}): React.ReactElement {
  const providers = useFlux((s) => s.providers);
  const catalog = useFlux((s) => s.providerModels[props.providerId]);
  const saved = useFlux((s) => s.savedModels).filter((m) => m.provider === props.providerId);
  const needsFetch = providers.length === 0;
  // The LOCAL saved-model list persists server-side but was only pulled by
  // the Providers dialog — a fresh session (new page / first server run)
  // opened the picker with an EMPTY model datalist even though models were
  // saved. First picker open pulls it (same lazy pattern as the registry
  // summary); a legitimately empty registry doesn't re-fire.
  const needsModels = useFlux((s) => s.savedModels.length === 0);

  // Registry summary: fetched once on first open (the store caches it).
  useEffect(() => {
    if (needsFetch) fetchProviders();
  }, [needsFetch]);

  // Saved models: fetched once on first open (the store caches it).
  useEffect(() => {
    if (needsModels) fetchModels();
  }, [needsModels]);

  // The hint follows the typed model: a SAVED row speaks first (its
  // editable params are the user's deployment truth), the probed catalog
  // falls back. Only known facts show — never a guessed number. The hint
  // is ONE line (the label carries only "required"); text-duplication
  // would break "get by text" queries and the eye.
  const savedHit = props.model ? saved.find((m) => m.model === props.model) : undefined;
  const probed = props.model ? catalog?.find((m) => m.id === props.model) : undefined;
  const ctx = savedHit?.params.context_length ?? probed?.context_length;
  const ctxHint = ctx ? fmtTokens(ctx) : undefined;
  const maxOut = savedHit?.params.max_tokens;
  const maxHint = maxOut ? fmtTokens(maxOut) : undefined;
  // No cached catalog yet (or the probe failed) → free-text with a pointer
  // to where catalogs are probed. NO live probe here, by design.
  const uncataloged = catalog === undefined || catalog.length === 0;
  // Datalist: saved rows first (star-marked), then catalog entries the
  // user has NOT saved. Datalist options carry no styling — the label
  // text is the only distinguishing voice.
  const savedIds = new Set(saved.map((m) => m.model));
  const catalogRest = (catalog ?? []).filter((m) => !savedIds.has(m.id));

  return (
    <div className="flex flex-col gap-1.5">
      {providers.length === 0 && (
        <p className="text-xs text-warn">
          No providers yet — add one via the Providers button in the top bar.
        </p>
      )}
      <label className="flex flex-col gap-1 text-sm text-muted">
        <span>Provider</span>
        <select
          className={cn(
            'h-[var(--fx-control-h)] cursor-pointer rounded-md border border-border bg-inset px-2.5 text-sm text-fg',
            // The one focus language: accent border (same as TextField) —
            // the ring variant made the picker's focus read differently
            // from every other input in the app.
            'transition-colors duration-100 focus:border-accent focus:outline-none',
            !props.providerId && 'text-muted',
          )}
          value={props.providerId}
          onChange={(e) => props.onChange({ provider: e.target.value, model: '' })}
        >
          {/* Prompt only — disabled, carries no selectable semantics. */}
          <option value="" disabled>
            Select a provider…
          </option>
          {providers.map((p) => (
            <option key={p.id} value={p.id}>
              {p.id}
            </option>
          ))}
        </select>
      </label>
      <label className="flex flex-col gap-1 text-sm text-muted">
        <span>
          Model
          <span className="ml-1 text-xs text-faint">
            (required{uncataloged && props.providerId ? ' · catalog in Providers' : ''})
          </span>
        </span>
        <input
          type="text"
          className={cn(
            'h-[var(--fx-control-h)] rounded-md border border-border bg-inset px-2.5 font-mono text-sm text-fg',
            'transition-colors duration-100 focus:border-accent focus:outline-none',
          )}
          value={props.model}
          placeholder="model id"
          list={`provider-models-${props.providerId || 'default'}`}
          onChange={(e) => props.onChange({ provider: props.providerId, model: e.target.value })}
        />
        <datalist id={`provider-models-${props.providerId || 'default'}`}>
          {saved.map((m) => (
            <option key={m.model} value={m.model}>
              {`★ ${m.meta.name || m.model}`}
            </option>
          ))}
          {catalogRest.map((m) => (
            <option key={m.id} value={m.id} />
          ))}
        </datalist>
        {(ctxHint || maxHint || savedHit) && (
          <span className="text-2xs text-faint" aria-live="polite">
            {[
              ctxHint ? `${ctxHint} ctx` : undefined,
              maxHint ? `${maxHint} max out` : undefined,
              savedHit ? 'saved — editable in Providers' : undefined,
            ]
              .filter(Boolean)
              .join(' · ')}
          </span>
        )}
      </label>
    </div>
  );
}
