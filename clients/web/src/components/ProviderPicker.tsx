/**
 * ProviderPicker.tsx — provider select + model input, shared by the
 * new-chat dialog and the composer's switcher.
 *
 * The list is the truth: only real registry entries render (there is no
 * synthetic "server default" option — `chat_create` requires an explicit
 * provider pin). The model input's datalist is the LOCAL SAVED registry
 * only — an upstream probed catalog runs to hundreds of ids and would
 * bury the handful the user actually deploys. Discovery/import lives in
 * the Providers dialog (the ONE place that probes); the input stays
 * free-text so an unsaved model string still pins (the registry is a
 * convenience, never a gate — upstream defaults apply). This picker
 * never issues a probe: creating a chat must not stall on upstream
 * round-trips; the probed cache only powers the per-typed-id context
 * hint.
 *
 * Provides: ProviderPicker
 * Depends: core/state.ts, services/providers.ts, components/ui/*
 */
import { useEffect } from 'react';
import { useFlux } from '../core/state';
import { fetchProviders } from '../services/providers';
import { fmtTokens } from '../lib/format';
import { SelectField, TextField } from './ui';

export function ProviderPicker(props: {
  providerId: string;
  model: string;
  onChange: (next: { provider: string; model: string }) => void;
}): React.ReactElement {
  const providers = useFlux((s) => s.providers);
  const catalog = useFlux((s) => s.providerModels[props.providerId]);
  const saved = useFlux((s) => s.savedModels).filter((m) => m.provider === props.providerId);
  const needsFetch = providers.length === 0;
  // The saved-model list is SESSION-level: preloaded at attach
  // (handlers.session_resumed) and refreshed by the mutation broadcasts —
  // the picker only reads it.

  // Registry summary: fetched once on first open (the store caches it).
  useEffect(() => {
    if (needsFetch) fetchProviders();
  }, [needsFetch]);

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
  // No saved rows for the picked provider → the datalist has nothing to
  // offer; free-text still works, the pointer names the import surface.
  const unimported = !!props.providerId && saved.length === 0;
  // Datalist: the SAVED rows only (★ display name). The upstream catalog
  // never rides here — see the module doc.

  return (
    <div className="flex flex-col gap-1.5">
      {providers.length === 0 && (
        <p className="text-xs text-warn">
          No providers yet — add one via the Providers button in the top bar.
        </p>
      )}
      <label className="flex flex-col gap-1 text-sm text-muted">
        <span>Provider</span>
        {/* One control language: SelectField owns the select chrome (same
            focus border as every other input in the app). */}
        <SelectField
          className={!props.providerId ? 'text-muted' : undefined}
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
        </SelectField>
      </label>
      <label className="flex flex-col gap-1 text-sm text-muted">
        <span>
          Model
          <span className="ml-1 text-xs text-faint">
            (required{unimported ? ' · import in Providers' : ''})
          </span>
        </span>
        <TextField
          type="text"
          className="font-mono"
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
