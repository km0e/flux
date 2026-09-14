/**
 * ProvidersPanel — the provider registry's management section of the
 * Settings dialog.
 *
 * The registry lives in the server database (there is no config file).
 * Entries are pure endpoints (id + effective url — the api_key NEVER
 * comes down the wire); the model is pinned per chat. Saving never
 * validates connectivity — instead, THIS panel owns the model-catalog
 * probe: every uncached catalog is probed once per section visit (the
 * chat pickers read the same cache and never probe ad hoc), and the
 * preview renders it with an explicit Refresh. A mutation ack implies
 * durability (the server persists first); the fresh list arrives via the
 * `providers` broadcast, so the panel never refetches after a mutation.
 *
 * Desktop: master-detail — a selection rail (a persistent "+ New provider"
 * row above the entries) and a detail pane that either previews the
 * selected entry or carries the creation form. Mobile keeps the stacked
 * layout (hint, rows with inline actions, form). The form draft lives at
 * panel level, so switching the selection never loses a half-typed entry;
 * after a successful add the new entry is auto-selected once its broadcast
 * lands.
 *
 * Presentation shares the integration-ui building blocks with McpPanel /
 * SkillsPanel — one typography scale, one spacing rhythm, one
 * two-step-remove control, one rail-selection repair.
 *
 * Provides: ProvidersPanel
 * Depends: core/state.ts, services/providers.ts, hooks/useIsMobile.ts,
 *          components/ui/*, components/dialogs/integration-ui.tsx
 */
import { useEffect, useState } from 'react';
import { ChevronDown, ChevronRight, CircleX, Download, RefreshCw } from 'lucide-react';
import { useFlux } from '../../core/state';
import { addProvider, fetchProviders, probeProvider, removeProvider } from '../../services/providers';
import { fetchModels, saveModel } from '../../services/models';
import { useIsMobile } from '../../hooks/useIsMobile';
import { fmtTokens } from '../../lib/format';
import type { ProviderModelInfo, ProviderSummary } from '../../core/types';
import { Badge, Button, IconButton, Spinner, TextField } from '../ui';
import { SavedModelsSection } from './ModelSection';
import {
  DetailPane,
  DialogHint,
  EmptyState,
  FormField,
  MasterDetail,
  NewRailButton,
  Rail,
  RailButton,
  RemoveControl,
  RowShell,
  RowSub,
  RowTitle,
  SectionLabel,
  useRailSelection,
} from './integration-ui';

/** The rail selection key of one provider row (stable reference). */
const keyOf = (p: ProviderSummary): string => p.id;

/** The creation form — shared verbatim by the desktop detail pane and the
 * mobile stacked layout (one set of field states, one submit). */
function ProviderForm(props: {
  id: string;
  url: string;
  apiKey: string;
  adding: boolean;
  canAdd: boolean;
  addError: string | null;
  setId: (v: string) => void;
  setUrl: (v: string) => void;
  setApiKey: (v: string) => void;
  onSubmit: () => void;
}): React.ReactElement {
  return (
    <form
      className="flex flex-col gap-3 rounded-lg border border-border bg-panel p-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (props.canAdd) props.onSubmit();
      }}
    >
      <SectionLabel>Add provider</SectionLabel>
      <div className="flex gap-2 max-md:flex-col">
        <FormField label="Id" hint="e.g. main" className="flex-1">
          <TextField value={props.id} onChange={(e) => props.setId(e.target.value)} placeholder="main" />
        </FormField>
        <FormField label="Base url" hint="blank = OpenAI default" className="flex-[1.8]">
          <TextField
            value={props.url}
            onChange={(e) => props.setUrl(e.target.value)}
            placeholder="https://api.openai.com/v1"
            className="font-mono text-sm"
          />
        </FormField>
      </div>
      <FormField label="API key" hint="stored in the server database, never shown back">
        <TextField
          type="password"
          value={props.apiKey}
          onChange={(e) => props.setApiKey(e.target.value)}
          placeholder="sk-…"
          autoComplete="off"
          className="font-mono text-sm"
        />
      </FormField>
      {props.addError && <span className="text-2xs break-all text-danger">{props.addError}</span>}
      <div className="flex justify-end">
        <Button variant="primary" type="submit" disabled={!props.canAdd}>
          {props.adding ? <Spinner /> : null}
          Add provider
        </Button>
      </div>
    </form>
  );
}

/** The probed catalog block — a filterable mono list (context length where
 * the upstream reports one) with per-row IMPORT affordances: an unsaved
 * entry imports (the server auto-fills models.dev metadata on create),
 * a saved one shows a mark (the conflict rule: local wins, no overwrite).
 * The filter is the discovery surface now that the chat pickers list only
 * SAVED models — upstream catalogs run to hundreds of ids and the picker
 * datalist must stay lean. Shared by the preview pane and the mobile
 * row's expanded section. */
function CatalogBlock(props: {
  id: string;
  catalog: ProviderModelInfo[];
  savedIds: Set<string>;
  importing: string | null;
  onImport: (modelId: string) => void;
}): React.ReactElement {
  const [filter, setFilter] = useState('');
  const needle = filter.trim().toLowerCase();
  const shown = needle
    ? props.catalog.filter((m) => m.id.toLowerCase().includes(needle))
    : props.catalog;
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <TextField
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="filter models…"
          aria-label={`Filter models of ${props.id}`}
          className="font-mono text-xs"
        />
        <span className="shrink-0 text-2xs text-faint tabular-nums">
          {shown.length === props.catalog.length
            ? `${props.catalog.length}`
            : `${shown.length} / ${props.catalog.length}`}
        </span>
      </div>
      <ul
        className="m-0 flex max-h-44 list-none flex-col gap-px overflow-y-auto rounded-sm border border-border bg-bg p-1.5"
        aria-label={`Models of ${props.id}`}
      >
        {shown.map((m) => {
          const saved = props.savedIds.has(m.id);
          return (
            <li
              key={m.id}
              className="flex items-baseline justify-between gap-3 px-1 py-0.5 font-mono text-xs text-fg"
            >
              <span className="min-w-0 truncate">{m.id}</span>
              <span className="flex shrink-0 items-baseline gap-2">
                {m.context_length !== undefined && (
                  <span className="text-2xs text-faint tabular-nums">
                    {fmtTokens(m.context_length)} ctx
                  </span>
                )}
                {saved ? (
                  <Badge tone="default" title="Already saved locally — import skips it">
                    saved
                  </Badge>
                ) : (
                  <button
                    type="button"
                    className="cursor-pointer rounded-sm border border-border px-1.5 py-0.5 text-2xs text-muted transition-colors duration-fast hover:border-border-strong hover:text-fg"
                    disabled={props.importing !== null}
                    onClick={() => props.onImport(m.id)}
                    title="Save this model (metadata auto-filled from models.dev)"
                  >
                    {props.importing === m.id ? 'importing…' : 'Import'}
                  </button>
                )}
              </span>
            </li>
          );
        })}
        {shown.length === 0 && <li className="px-1 py-1 text-2xs text-faint">no match</li>}
      </ul>
    </div>
  );
}

/** The import-all flow for one provider's catalog: creates every unsaved
 * entry via model_save (the server enriches each create from models.dev).
 * Conflict rule: EXISTING saved rows win — skipped, never overwritten,
 * and the outcome toast says so. Sequential: one ack at a time keeps the
 * toast counts honest. */
async function importAllCatalog(pid: string): Promise<void> {
  const catalog = useFlux.getState().providerModels[pid] ?? [];
  const saved = useFlux.getState().savedModels;
  const savedIds = new Set(saved.filter((m) => m.provider === pid).map((m) => m.model));
  const fresh = catalog.filter((m) => !savedIds.has(m.id));
  const skipped = catalog.length - fresh.length;
  let imported = 0;
  let enriched = 0;
  let firstError: string | undefined;
  for (const m of fresh) {
    const r = await saveModel(pid, m.id, {});
    if (r.error) {
      firstError = r.error;
      break;
    }
    imported += 1;
    if (r.enriched) enriched += 1;
  }
  const toast = useFlux.getState();
  if (firstError) {
    toast.pushToast('error', `Import stopped after ${imported}: ${firstError}`);
    return;
  }
  const parts = [`Imported ${imported}`];
  if (enriched) parts.push(`${enriched} filled from models.dev`);
  if (skipped) parts.push(`${skipped} skipped — saved models kept`);
  toast.pushToast('info', parts.join(' · '));
}

/** Mobile row: compact, with the inline actions the stacked layout needs
 * (no detail pane exists there). */
function ProviderRow(props: {
  id: string;
  url: string;
  probing: boolean;
  catalog: ProviderModelInfo[] | undefined;
  probeError: string | undefined;
  onRefresh: () => void;
  onRemove: () => Promise<string | undefined>;
  /** Import surface (owned by the panel so the toast counts stay honest):
   * saved model ids, the in-flight import ("pid/model" | "pid/*"), and
   * the two entry points. */
  savedIds: Set<string>;
  importing: string | null;
  onImport: (modelId: string) => void;
  importingAll: boolean;
  onImportedAll: () => void;
}): React.ReactElement {
  const [expanded, setExpanded] = useState(false);
  const probed = props.catalog !== undefined && !props.probing && !props.probeError;
  const expandable = !!props.catalog?.length;
  return (
    <RowShell>
      <div className="flex items-center gap-2">
        <IconButton
          label={expanded ? `Hide models of ${props.id}` : `Show models of ${props.id}`}
          aria-expanded={expanded}
          disabled={!expandable}
          className="size-6 disabled:opacity-35"
          onClick={() => setExpanded((v) => !v)}
        >
          {expanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
        </IconButton>
        <RowTitle>{props.id}</RowTitle>
        {props.probing && <Badge tone="accent">probing…</Badge>}
        {probed && (
          <Badge
            tone={props.catalog!.length ? 'accent' : 'default'}
            title={props.catalog!.length ? 'Probed model catalog (cached)' : 'Upstream answered without a catalog'}
          >
            {props.catalog!.length ? `${props.catalog!.length} models` : 'no catalog'}
          </Badge>
        )}
        {props.probeError && (
          <Badge tone="warn" title={props.probeError}>
            <CircleX size={10} className="mr-0.5 inline" />
            unreachable
          </Badge>
        )}
        <Button variant="ghost" size="sm" disabled={props.probing} onClick={props.onRefresh} title="Probe the upstream model catalog">
          <RefreshCw size={11} className={props.probing ? 'animate-spin' : undefined} />
          Refresh
        </Button>
        <span className="flex-1" />
        <RemoveControl label={`Remove ${props.id}`} onRemove={props.onRemove} />
      </div>
      <RowSub title={props.url}>{props.url}</RowSub>
      {props.probeError && <span className="text-2xs break-all text-warn">{props.probeError}</span>}
      {expanded && (
        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <span className="text-2xs text-faint">Upstream catalog</span>
            <span className="flex-1" />
            <Button
              variant="ghost"
              size="sm"
              disabled={props.importingAll}
              title="Save every unsaved catalog model (metadata auto-filled from models.dev)"
              onClick={props.onImportedAll}
            >
              {props.importingAll ? <Spinner /> : <Download size={11} aria-hidden="true" />}
              Import all
            </Button>
          </div>
          <CatalogBlock
            id={props.id}
            catalog={props.catalog!}
            savedIds={props.savedIds}
            importing={props.importing}
            onImport={props.onImport}
          />
          <SavedModelsSection provider={props.id} />
        </div>
      )}
    </RowShell>
  );
}

/** Desktop detail: the selected entry's preview — id, status badges, the
 * SAVED models section (edit/params/sync), the catalog with import
 * affordances, refresh, and the two-step remove. Keyed by id so
 * confirm/error state resets per selection. */
function ProviderPreview(props: {
  id: string;
  probing: boolean;
  onRefresh: () => void;
  onRemove: () => Promise<string | undefined>;
  savedIds: Set<string>;
  importing: string | null;
  onImport: (modelId: string) => void;
  importingAll: boolean;
  onImportedAll: () => void;
}): React.ReactElement {
  const p = useFlux((s) => s.providers.find((x) => x.id === props.id));
  const catalog = useFlux((s) => s.providerModels[props.id]);
  const probeError = useFlux((s) => s.providerProbeErrors[props.id]);
  if (!p) return <EmptyState>Provider removed.</EmptyState>;
  const probed = catalog !== undefined && !props.probing && !probeError;
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border bg-panel p-4">
      <div className="flex items-center gap-2">
        <RowTitle>{p.id}</RowTitle>
        {props.probing && <Badge tone="accent">probing…</Badge>}
        {probed && (
          <Badge
            tone={catalog!.length ? 'accent' : 'default'}
            title={catalog!.length ? 'Probed model catalog (cached)' : 'Upstream answered without a catalog'}
          >
            {catalog!.length ? `${catalog!.length} models` : 'no catalog'}
          </Badge>
        )}
        {probeError && (
          <Badge tone="warn" title={probeError}>
            <CircleX size={10} className="mr-0.5 inline" />
            unreachable
          </Badge>
        )}
        <span className="flex-1" />
        <Button variant="ghost" size="sm" disabled={props.probing} onClick={props.onRefresh} title="Probe the upstream model catalog">
          <RefreshCw size={11} className={props.probing ? 'animate-spin' : undefined} />
          Refresh
        </Button>
        <RemoveControl label={`Remove ${p.id}`} onRemove={props.onRemove} />
      </div>
      <RowSub title={p.url}>{p.url}</RowSub>
      <span className="text-2xs text-faint">API key stored in the server database — never shown back.</span>
      {probeError && <span className="text-2xs break-all text-warn">{probeError}</span>}
      <SavedModelsSection provider={p.id} />
      {catalog && catalog.length > 0 && (
        <div className="flex flex-col gap-1.5">
          <div className="flex items-center gap-2">
            <span className="text-2xs text-faint">Upstream catalog</span>
            <span className="flex-1" />
            <Button
              variant="ghost"
              size="sm"
              disabled={props.importingAll}
              title="Save every unsaved catalog model (metadata auto-filled from models.dev)"
              onClick={props.onImportedAll}
            >
              {props.importingAll ? <Spinner /> : <Download size={11} aria-hidden="true" />}
              Import all
            </Button>
          </div>
          <CatalogBlock
            id={p.id}
            catalog={catalog}
            savedIds={props.savedIds}
            importing={props.importing}
            onImport={props.onImport}
          />
        </div>
      )}
    </div>
  );
}

const HINT =
  'Pure endpoints (id · url · api key) stored in the server database — the model is ' +
  'pinned per chat. Saving never validates connectivity: each provider probes its model ' +
  'catalog when the section opens, and importing a catalog model saves it locally with ' +
  'metadata auto-filled from models.dev (already-saved models win — never overwritten). ' +
  'Removing a provider in use breaks its chats\u2019 next round (recovery: switch the ' +
  'chat\u2019s provider).';

export function ProvidersPanel(): React.ReactElement {
  const providers = useFlux((s) => s.providers);
  const catalogMap = useFlux((s) => s.providerModels);
  const probeErrors = useFlux((s) => s.providerProbeErrors);
  const isMobile = useIsMobile();
  const { selected, choose, markPending, noteRemoved } = useRailSelection(providers, keyOf);

  // In-flight catalog probes — panel-owned so the rail rows, the preview
  // and the mobile rows all render the same truth.
  const [probingIds, setProbingIds] = useState<string[]>([]);

  // In-flight catalog IMPORT ("pid/model" for a single row, "pid/*" for
  // import-all) — panel-owned for the same reason as the probes.
  const [importing, setImporting] = useState<string | null>(null);

  // The creation-form draft lives at panel level: switching the selection
  // never loses a half-typed entry.
  const [id, setId] = useState('');
  const [url, setUrl] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  // The lists may be cold (the pickers fetch lazily) — pull on section show.
  useEffect(() => {
    fetchProviders();
    fetchModels();
  }, []);

  /** Import one catalog row: a create via model_save — the server
   * auto-fills models.dev metadata. Local rows are never touched. */
  const importOne = (pid: string, modelId: string) => {
    if (importing !== null) return;
    setImporting(`${pid}/${modelId}`);
    void saveModel(pid, modelId, {}).then((r) => {
      setImporting(null);
      if (r.error) {
        useFlux.getState().pushToast('error', `Import of "${modelId}" failed: ${r.error}`);
      } else {
        useFlux
          .getState()
          .pushToast(
            'info',
            `Model "${modelId}" saved${r.enriched ? ' · filled from models.dev' : ''}`,
          );
      }
    });
  };

  /** Import every unsaved catalog row of one provider (conflict rule:
   * saved rows win — skipped with a hint, never overwritten). */
  const importAll = (pid: string) => {
    if (importing !== null) return;
    setImporting(`${pid}/*`);
    void importAllCatalog(pid).then(() => setImporting(null));
  };

  /** Probe one catalog, tracking the in-flight id for the status badges. */
  const refresh = (pid: string) => {
    setProbingIds((prev) => (prev.includes(pid) ? prev : [...prev, pid]));
    void probeProvider(pid).finally(() => setProbingIds((prev) => prev.filter((x) => x !== pid)));
  };

  // Probe every uncached catalog once per section visit — the chat pickers
  // read this same cache, so viewing the section warms it for them.
  useEffect(() => {
    for (const p of useFlux.getState().providers) {
      if (useFlux.getState().providerModels[p.id] !== undefined) continue;
      refresh(p.id);
    }
    // once per mount — the cache check owns the skip decision
  }, []);

  const canAdd = id.trim() !== '' && !adding;

  const runAdd = () => {
    setAdding(true);
    setAddError(null);
    const added = id.trim();
    void addProvider({ id, url, api_key: apiKey }).then((error) => {
      setAdding(false);
      if (error) {
        setAddError(error);
        return;
      }
      // Success: the broadcast refreshes the list (and lands the pending
      // selection) — reset the form.
      markPending(added);
      setId('');
      setUrl('');
      setApiKey('');
      useFlux.getState().pushToast('info', `Provider "${added}" added`);
    });
  };

  /** The saved-model id set of one provider (the catalog's import marks). */
  const savedIdsFor = (pid: string): Set<string> =>
    new Set(useFlux.getState().savedModels.filter((m) => m.provider === pid).map((m) => m.model));

  /** Shared by the preview pane and the mobile rows. */
  const remove = (pid: string): Promise<string | undefined> => {
    noteRemoved(pid);
    return removeProvider(pid).then((error) => {
      if (!error) useFlux.getState().pushToast('info', `Provider "${pid}" removed`);
      return error;
    });
  };

  const selectedProvider = selected === 'new' ? undefined : providers.find((p) => keyOf(p) === selected);

  // ── Mobile: the stacked layout (hint, rows with inline actions, form) ──
  if (isMobile) {
    return (
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
        <DialogHint>{HINT}</DialogHint>
        <div className="flex flex-col gap-2">
          <SectionLabel>Registered</SectionLabel>
          {providers.length === 0 ? (
            <EmptyState>No providers yet — add one below.</EmptyState>
          ) : (
            <ul aria-label="Providers" className="m-0 flex list-none flex-col gap-2 p-0">
              {providers.map((p) => (
                <ProviderRow
                  key={p.id}
                  id={p.id}
                  url={p.url}
                  probing={probingIds.includes(p.id)}
                  catalog={catalogMap[p.id]}
                  probeError={probeErrors[p.id]}
                  onRefresh={() => refresh(p.id)}
                  onRemove={() => remove(p.id)}
                  savedIds={savedIdsFor(p.id)}
                  importing={importing}
                  onImport={(modelId) => importOne(p.id, modelId)}
                  importingAll={importing === `${p.id}/*`}
                  onImportedAll={() => importAll(p.id)}
                />
              ))}
            </ul>
          )}
        </div>
        <ProviderForm
          id={id}
          url={url}
          apiKey={apiKey}
          adding={adding}
          canAdd={canAdd}
          addError={addError}
          setId={setId}
          setUrl={setUrl}
          setApiKey={setApiKey}
          onSubmit={runAdd}
        />
      </div>
    );
  }

  // ── Desktop: master-detail — rail of entries + preview/form detail ──
  return (
    <MasterDetail
      list={
        <Rail label="Providers">
          <NewRailButton label="New provider" selected={selected === 'new'} onClick={() => choose('new')} />
          {providers.map((p) => (
            <RailButton
              key={p.id}
              selected={selected === p.id}
              onClick={() => choose(p.id)}
              ariaLabel={`Select provider ${p.id}`}
              title={p.id}
              sub={p.url}
            />
          ))}
          {providers.length === 0 && (
            <li className="px-2 py-1 text-2xs leading-relaxed text-faint">No providers yet.</li>
          )}
        </Rail>
      }
      detail={
        <DetailPane>
          {selectedProvider ? (
            <ProviderPreview
              key={selectedProvider.id}
              id={selectedProvider.id}
              probing={probingIds.includes(selectedProvider.id)}
              onRefresh={() => refresh(selectedProvider.id)}
              onRemove={() => remove(selectedProvider.id)}
              savedIds={savedIdsFor(selectedProvider.id)}
              importing={importing}
              onImport={(modelId) => importOne(selectedProvider.id, modelId)}
              importingAll={importing === `${selectedProvider.id}/*`}
              onImportedAll={() => importAll(selectedProvider.id)}
            />
          ) : (
            <>
              <DialogHint>{HINT}</DialogHint>
              <ProviderForm
                id={id}
                url={url}
                apiKey={apiKey}
                adding={adding}
                canAdd={canAdd}
                addError={addError}
                setId={setId}
                setUrl={setUrl}
                setApiKey={setApiKey}
                onSubmit={runAdd}
              />
            </>
          )}
        </DetailPane>
      }
    />
  );
}
