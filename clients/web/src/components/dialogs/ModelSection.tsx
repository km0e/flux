/**
 * ModelSection.tsx — the LOCAL saved-model registry section of the
 * Providers panel (shared verbatim by the desktop preview pane and the
 * mobile stacked rows).
 *
 * The registry lives in the server database; this section lists the saved
 * models of one provider (editable request params + the models.dev
 * metadata snapshot the server auto-filled at import), with an inline
 * params editor, a two-step remove, and a provider-scoped models.dev
 * re-sync. The CATALOG import (probe → saved row) stays in
 * ProvidersPanel — the conflict rule (local wins, hint shown) is applied
 * there where the batch context lives.
 *
 * Param editors pair a free number input with preset chips ("common
 * options"); when the models.dev snapshot carries the value, a hint names
 * it and the editor pre-fills from it on first edit. A snapshot that says
 * the model has no temperature support soft-locks the temperature field
 * (overridable — the snapshot is advisory, not a gate).
 *
 * Provides: SavedModelsSection
 * Depends: core/state.ts, services/models.ts, lib/format.ts,
 *          components/ui, components/dialogs/integration-ui.tsx
 */
import { useState } from 'react';
import { RefreshCw } from 'lucide-react';
import { useFlux } from '../../core/state';
import { removeModel, saveModel, syncModels } from '../../services/models';
import { fmtTokens } from '../../lib/format';
import type { ModelParams, SavedModelInfo } from '../../core/types';
import { Badge, Button, Spinner, TextField } from '../ui';
import { RemoveControl, RowSub, SectionLabel } from './integration-ui';

/** Common-value presets ("常见选项") — static frontend constants; the
 * models.dev snapshot refines the defaults per model where it can. */
const CONTEXT_PRESETS = [8192, 32768, 65536, 128000, 200000, 1000000];
const MAX_TOKENS_PRESETS = [4096, 8192, 16384, 32768, 65536];
const TEMPERATURE_PRESETS = [0, 0.3, 0.7, 1.0];
const TOP_P_PRESETS = [0.9, 0.95, 1];

/** One labeled number field with preset chips. Empty string = unset. */
function ParamNumberField(props: {
  label: string;
  hint?: string;
  value: number | undefined;
  presets: number[];
  warn?: string;
  onChange: (v: number | undefined) => void;
}): React.ReactElement {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-xs text-muted">
        {props.label}
        {props.hint && <span className="ml-1 text-2xs text-faint">{props.hint}</span>}
      </span>
      <TextField
        type="number"
        inputMode="decimal"
        className="font-mono text-sm"
        value={props.value ?? ''}
        placeholder="unset"
        aria-label={props.label}
        onChange={(e) => {
          const raw = e.target.value;
          props.onChange(raw === '' ? undefined : Number(raw));
        }}
      />
      {props.warn && <span className="text-2xs text-warn">{props.warn}</span>}
      <div className="flex flex-wrap gap-1" role="group" aria-label={`${props.label} presets`}>
        {props.presets.map((p) => (
          <button
            key={p}
            type="button"
            className={cnChip(props.value === p)}
            onClick={() => props.onChange(props.value === p ? undefined : p)}
          >
            {fmtTokens(p)}
          </button>
        ))}
      </div>
    </div>
  );
}

function cnChip(active: boolean): string {
  return (
    'cursor-pointer rounded-sm border px-1.5 py-0.5 font-mono text-2xs tabular-nums transition-colors duration-fast ' +
    (active
      ? 'border-accent bg-accent-dim text-accent'
      : 'border-border bg-inset text-muted hover:border-border-strong hover:text-fg')
  );
}

/** The inline params editor. The draft starts from the row's params —
 * create starts empty, edit keeps what the user set. */
function ModelEditForm(props: {
  provider: string;
  model: string;
  initial: ModelParams;
  meta: SavedModelInfo['meta'];
  onDone: (error?: string) => void;
}): React.ReactElement {
  const [draft, setDraft] = useState<ModelParams>({ ...props.initial });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const patch = (p: Partial<ModelParams>) => setDraft((d) => ({ ...d, ...p }));
  // Advisory: models.dev says this model has no temperature control.
  const tempUnsupported = props.meta.temperature === false;

  const submit = () => {
    setSaving(true);
    setError(null);
    void saveModel(props.provider, props.model, draft).then((r) => {
      setSaving(false);
      if (r.error) setError(r.error);
      else props.onDone();
    });
  };

  return (
    <form
      className="flex flex-col gap-3 rounded-md border border-border bg-bg p-3"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
        <ParamNumberField
          label="Context length"
          hint={props.meta.context_length ? `(models.dev: ${fmtTokens(props.meta.context_length)})` : undefined}
          value={draft.context_length}
          presets={CONTEXT_PRESETS}
          onChange={(v) => patch({ context_length: v })}
        />
        <ParamNumberField
          label="Max output tokens"
          hint={props.meta.max_output ? `(models.dev: ${fmtTokens(props.meta.max_output)})` : undefined}
          value={draft.max_tokens}
          presets={MAX_TOKENS_PRESETS}
          onChange={(v) => patch({ max_tokens: v })}
        />
        <ParamNumberField
          label="Temperature"
          hint={props.meta.temperature ? 'supported' : undefined}
          warn={tempUnsupported ? 'models.dev: no temperature support' : undefined}
          value={draft.temperature}
          presets={TEMPERATURE_PRESETS}
          onChange={(v) => patch({ temperature: v })}
        />
        <ParamNumberField
          label="Top P"
          value={draft.top_p}
          presets={TOP_P_PRESETS}
          onChange={(v) => patch({ top_p: v })}
        />
      </div>
      <span className="text-2xs leading-relaxed text-faint">
        Unset fields are omitted — the upstream default applies. Edits never touch the
        models.dev snapshot; a failed round names a bad value.
      </span>
      {error && <span className="break-all text-2xs text-danger">{error}</span>}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="ghost" size="sm" onClick={() => props.onDone()}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" size="sm" disabled={saving}>
          {saving ? <Spinner /> : null}
          Save
        </Button>
      </div>
    </form>
  );
}

/** One saved-model row: identity + capability/price badges + edit/remove.
 * When the models.dev snapshot matched a DIFFERENT catalog id (spelling
 * or snapshot-suffix normalization), the matched entry is named inline —
 * a wrong match is always visible, never silent. */
function SavedModelRow(props: { provider: string; row: SavedModelInfo }): React.ReactElement {
  const [editing, setEditing] = useState(false);
  const meta = props.row.meta;
  const cost = meta.cost;
  const matched = meta.models_dev;
  const aliased = matched !== undefined && matched.model !== props.row.model;

  return (
    <li className="flex flex-col gap-1.5 rounded-md border border-border bg-inset px-2.5 py-2">
      <div className="flex items-center gap-2">
        <span className="min-w-0 truncate font-mono text-xs text-fg" title={props.row.model}>
          {props.row.model}
        </span>
        {meta.name && <RowSub>{meta.name}</RowSub>}
        {meta.source === 'models.dev' && (
          <Badge
            tone="accent"
            title={
              matched
                ? `Metadata from models.dev — matched ${matched.provider}/${matched.model}`
                : 'Metadata auto-filled from models.dev'
            }
          >
            models.dev
          </Badge>
        )}
        <span className="flex-1" />
        <Button variant="ghost" size="sm" onClick={() => setEditing((v) => !v)}>
          {editing ? 'Close' : 'Edit'}
        </Button>
        <RemoveControl
          label={`Remove ${props.row.model}`}
          onRemove={() =>
            removeModel(props.provider, props.row.model).then((error) => {
              if (!error) {
                useFlux.getState().pushToast('info', `Model "${props.row.model}" removed`);
              }
              return error;
            })
          }
        />
      </div>
      <div className="flex flex-wrap items-center gap-1.5">
        {(props.row.params.context_length ?? meta.context_length) !== undefined && (
          <Badge tone="default">
            {fmtTokens(props.row.params.context_length ?? meta.context_length!)} ctx
          </Badge>
        )}
        {(props.row.params.max_tokens ?? meta.max_output) !== undefined && (
          <Badge tone="default">
            {fmtTokens(props.row.params.max_tokens ?? meta.max_output!)} max out
          </Badge>
        )}
        {meta.reasoning && <Badge tone="accent">reasoning</Badge>}
        {meta.tool_call && <Badge tone="default">tools</Badge>}
        {meta.knowledge && <RowSub>knowledge {meta.knowledge}</RowSub>}
        {aliased && (
          <RowSub title={`models.dev entry: ${matched.provider}/${matched.model}`}>
            matched: {matched.provider}/{matched.model}
          </RowSub>
        )}
      </div>
      {cost && (
        <RowSub>
          ${(cost.input ?? 0)}/M in · ${(cost.output ?? 0)}/M out
          {cost.cache_read !== undefined && ` · $${cost.cache_read}/M cache`}
        </RowSub>
      )}
      {editing && (
        <ModelEditForm
          provider={props.provider}
          model={props.row.model}
          initial={props.row.params}
          meta={meta}
          onDone={() => setEditing(false)}
        />
      )}
    </li>
  );
}

/** The saved-models section for one provider: header (count + sync),
 * rows, and an empty-state line. */
export function SavedModelsSection(props: { provider: string }): React.ReactElement {
  const rows = useFlux((s) => s.savedModels).filter((m) => m.provider === props.provider);
  const [syncing, setSyncing] = useState(false);

  const runSync = () => {
    setSyncing(true);
    void syncModels(props.provider).then((r) => {
      setSyncing(false);
      if (r.error) {
        useFlux.getState().pushToast('error', `models.dev sync failed: ${r.error}`);
      } else {
        useFlux
          .getState()
          .pushToast(
            'info',
            r.updated
              ? `Synced ${r.updated} model${r.updated === 1 ? '' : 's'} from models.dev`
              : 'models.dev sync — everything already current',
          );
      }
    });
  };

  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center gap-2">
        <SectionLabel>Saved models ({rows.length})</SectionLabel>
        <span className="flex-1" />
        <Button
          variant="ghost"
          size="sm"
          disabled={syncing}
          onClick={runSync}
          title="Re-fetch metadata from models.dev (params are preserved)"
        >
          {syncing ? (
            <Spinner />
          ) : (
            <RefreshCw size={11} aria-hidden="true" />
          )}
          Sync
        </Button>
      </div>
      {rows.length === 0 ? (
        <span className="text-2xs text-faint">
          No saved models yet — import from the catalog below, or save one from a picker.
        </span>
      ) : (
        <ul aria-label={`Saved models of ${props.provider}`} className="m-0 flex list-none flex-col gap-1.5 p-0">
          {rows.map((row) => (
            <SavedModelRow key={row.model} provider={props.provider} row={row} />
          ))}
        </ul>
      )}
    </div>
  );
}
