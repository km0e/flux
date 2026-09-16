/**
 * McpPanel — the MCP launch list's management section of the Settings
 * dialog.
 *
 * The list lives in the server database (the server has no config file);
 * a mutation is persist-first and applied LIVE: the server connects the
 * server (a local stdio child process, or a remote Streamable HTTP
 * endpoint via the form's transport picker), registers its tools, and
 * rebuilds the running conversation engines so every chat's next round
 * sees the fresh tool set (a live round finishes first — a failed
 * connect rides the ack inline while the row stays and is retried at the
 * next server start). Summaries carry env and header KEYS only — values
 * never leave the server; editing an entry = delete + re-add.
 *
 * Desktop: master-detail — a selection rail (a persistent "+ New server"
 * row above the entries) and a detail pane that either previews the
 * selected entry (launch line, env keys, live state, remove) or carries
 * the creation form with the full explanation. Mobile keeps
 * the stacked layout (hint, notice, rows with inline actions, form). The
 * form draft lives at panel level, so switching the selection never
 * loses a half-typed entry; after a successful add the new entry is
 * auto-selected once its broadcast lands.
 *
 * Presentation shares the shared building blocks (settings/shared) with
 * ProvidersPanel / SkillsPanel — one typography scale, one spacing
 * rhythm, one two-step-remove control, one rail-selection repair.
 *
 * Provides: McpPanel
 * Depends: core/state.ts, services/mcp.ts, hooks/useIsMobile.ts,
 *          components/ui/*, components/settings/shared.tsx
 */
import { useEffect, useState } from 'react';
import { useFlux } from '../../core/state';
import { addMcpServer, fetchMcpServers, removeMcpServer } from '../../services/mcp';
import { useIsMobile } from '../../hooks/useIsMobile';
import { cn } from '../../lib/cn';
import { Badge, Button, Spinner, TextArea, TextField } from '../ui';
import type { McpKind, McpServerSummary, McpState, McpToolRegistration } from '../../core/types';
import {
  DetailPane,
  DialogHint,
  EmptyState,
  FormField,
  MasterDetail,
  NewRailButton,
  NoticeBar,
  Rail,
  RailButton,
  RemoveControl,
  RowShell,
  RowSub,
  RowTitle,
  SectionLabel,
  useRailSelection,
} from './shared';

/** The rail selection key of one server row (stable reference). */
const keyOf = (s: McpServerSummary): string => s.id;

/** The self-healing supervisor's view of one entry — a colored dot plus
 * the state word (Backoff/Offline are the interesting ones; Running is
 * the quiet default). */
function StateBadge({ state }: { state: McpState }): React.ReactElement {
  const tone =
    state === 'running'
      ? 'bg-success'
      : state === 'backoff'
        ? 'bg-warn'
        : 'bg-faint';
  const label = state === 'unspecified' ? 'unknown' : state;
  return (
    <span className="inline-flex items-center gap-1 text-2xs text-muted" title={`Self-healing state: ${label}`}>
      <span className={`inline-block size-1.5 rounded-full ${tone}`} aria-hidden />
      {label}
    </span>
  );
}

/** Parse a "one per line" textarea into a trimmed non-empty list. */
function parseLines(text: string): string[] {
  return text
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean);
}

/** Parse "KEY=VALUE" lines into an env object; returns an error string for
 * a malformed line instead of an entry. */
function parseEnv(text: string): { env: Record<string, string>; error?: string } {
  const env: Record<string, string> = {};
  for (const line of parseLines(text)) {
    const eq = line.indexOf('=');
    if (eq <= 0) return { env, error: `malformed line (expected KEY=VALUE): ${line}` };
    env[line.slice(0, eq).trim()] = line.slice(eq + 1).trim();
  }
  return { env };
}

/** The kind picker — a two-segment control in the app's segmented-tab
 * language (inset track, the active segment raised). */
function KindPicker(props: { kind: McpKind; onChange: (k: McpKind) => void }): React.ReactElement {
  const seg = (k: McpKind, label: string, title: string) => (
    <button
      type="button"
      aria-pressed={props.kind === k}
      title={title}
      onClick={() => props.onChange(k)}
      className={cn(
        'flex-1 cursor-pointer rounded-sm px-2.5 py-1 font-medium text-muted select-none',
        'transition-colors duration-fast hover:text-fg',
        props.kind === k && 'bg-elev text-fg shadow-sm',
      )}
    >
      {label}
    </button>
  );
  return (
    <div role="group" aria-label="Transport" className="flex gap-0.5 rounded-md bg-inset p-0.5">
      {seg('stdio', 'Command', 'Spawn a local child process over stdio')}
      {seg('http', 'URL', 'Connect to a remote Streamable HTTP endpoint')}
    </div>
  );
}

/** The creation form — shared verbatim by the desktop detail pane and the
 * mobile stacked layout. The stdio fields (command/args/env) and the http
 * fields (url/headers) are kind-conditional; the shared parts (id, kind,
 * submit) stay put so switching kind keeps the typed id. */
function McpForm(props: {
  id: string;
  kind: McpKind;
  command: string;
  argsText: string;
  envText: string;
  urlText: string;
  headersText: string;
  adding: boolean;
  canAdd: boolean;
  addError: string | null;
  setId: (v: string) => void;
  setKind: (k: McpKind) => void;
  setCommand: (v: string) => void;
  setArgsText: (v: string) => void;
  setEnvText: (v: string) => void;
  setUrlText: (v: string) => void;
  setHeadersText: (v: string) => void;
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
      <SectionLabel>Add MCP server</SectionLabel>
      <div className="flex gap-2 max-md:flex-col">
        <FormField label="Id" hint="e.g. filesystem" className="flex-1">
          <TextField value={props.id} onChange={(e) => props.setId(e.target.value)} placeholder="filesystem" />
        </FormField>
        <FormField label="Transport" hint="local child process, or remote Streamable HTTP" className="flex-1">
          <KindPicker kind={props.kind} onChange={props.setKind} />
        </FormField>
      </div>
      {props.kind === 'stdio' ? (
        <>
          <FormField label="Command" hint="the executable to spawn">
            <TextField
              value={props.command}
              onChange={(e) => props.setCommand(e.target.value)}
              placeholder="npx"
              className="font-mono text-sm"
            />
          </FormField>
          <FormField label="Arguments" hint="one per line">
            <TextArea
              aria-label="Arguments"
              rows={4}
              placeholder={'-y\n@modelcontextprotocol/server-filesystem\n/path/to/workspace'}
              value={props.argsText}
              onChange={(e) => props.setArgsText(e.target.value)}
            />
          </FormField>
          <FormField
            label="Environment"
            hint="KEY=VALUE per line — the child gets ONLY these (nothing inherited); values are stored server-side and never echoed back"
          >
            <TextArea
              aria-label="Environment"
              rows={3}
              placeholder={'SOME_VAR=value\nAPI_TOKEN=… (the value never leaves the server)'}
              value={props.envText}
              onChange={(e) => props.setEnvText(e.target.value)}
            />
          </FormField>
        </>
      ) : (
        <>
          <FormField label="URL" hint="the Streamable HTTP endpoint (http/https)">
            <TextField
              value={props.urlText}
              onChange={(e) => props.setUrlText(e.target.value)}
              placeholder="https://example.com/mcp"
              className="font-mono text-sm"
            />
          </FormField>
          <FormField
            label="Headers"
            hint="KEY=VALUE per line — sent with every request; auth rides here (Authorization=Bearer …); values are stored server-side and never echoed back"
          >
            <TextArea
              aria-label="Headers"
              rows={3}
              placeholder={'Authorization=Bearer … (the value never leaves the server)'}
              value={props.headersText}
              onChange={(e) => props.setHeadersText(e.target.value)}
            />
          </FormField>
        </>
      )}
      {props.addError && <span className="text-2xs break-all text-danger">{props.addError}</span>}
      <div className="flex justify-end">
        <Button variant="primary" type="submit" disabled={!props.canAdd}>
          {props.adding ? <Spinner /> : null}
          Add server
        </Button>
      </div>
    </form>
  );
}

/** The per-tool outcome block for a fresh add — the debugging surface
 * that keeps third-party integration off the server log: registered
 * tools ride the summary's tool_names, the SKIPPED ones (name
 * collisions) carry their reason here. */
function AddResults({ results }: { results: McpToolRegistration[] }): React.ReactElement {
  const skipped = results.filter((r) => !r.registered);
  if (results.length === 0) return <></>;
  return (
    <div className="flex flex-col gap-0.5 rounded-sm border border-border bg-bg px-2 py-1.5" role="status">
      <span className="text-2xs text-faint">
        Last add: {results.length - skipped.length} registered
        {skipped.length > 0 ? ` · ${skipped.length} skipped` : ''}
      </span>
      {skipped.map((r) => (
        <span key={r.name} className="text-2xs text-warn" title={r.reason}>
          ⊘ {r.name} — {r.reason}
        </span>
      ))}
    </div>
  );
}

/** The tool-name chips of one live entry, with the empty-state words the
 * state implies (no live session ≠ connected but toolless). */
function ToolChips({ server }: { server: McpServerSummary }): React.ReactElement {
  if (server.tool_names.length > 0) {
    return (
      <div className="flex flex-wrap gap-1" aria-label={`Tools of ${server.id}`}>
        {server.tool_names.map((t) => (
          <Badge key={t} title="Registered into the global tool registry">
            {t}
          </Badge>
        ))}
      </div>
    );
  }
  const word =
    server.state === 'offline'
      ? 'No live session — nothing registered yet (retried at the next server start).'
      : server.state === 'backoff'
        ? 'Reconnecting — tools resume when the session is back.'
        : 'Connected; the server registered no tools.';
  return <span className="text-2xs text-faint">{word}</span>;
}

/** The connect line of one entry — the launch line for stdio, the
 * endpoint URL for http. */
function connectLine(s: { kind: McpKind; command: string; args: string[]; url: string }): string {
  return s.kind === 'http' ? s.url : [s.command, ...s.args].join(' ');
}

/** Mobile row: compact, with the inline actions the stacked layout needs. */
function McpRow(props: {
  id: string;
  kind: McpKind;
  command: string;
  args: string[];
  url: string;
  envKeys: string[];
  headerKeys: string[];
  state: McpState;
  toolNames: string[];
  onRemove: () => Promise<string | undefined>;
}): React.ReactElement {
  const launch = connectLine(props);
  return (
    <RowShell>
      <div className="flex items-center gap-2">
        <RowTitle>{props.id}</RowTitle>
        <StateBadge state={props.state} />
        {props.envKeys.map((k) => (
          <Badge key={k} title={`env key ${k} (the value never leaves the server)`}>
            {k}
          </Badge>
        ))}
        {props.headerKeys.map((k) => (
          <Badge key={k} title={`header ${k} (the value never leaves the server)`}>
            {k}
          </Badge>
        ))}
        {props.toolNames.length > 0 && (
          <Badge title={props.toolNames.join(', ')}>{props.toolNames.length} tools</Badge>
        )}
        <span className="flex-1" />
        <RemoveControl label={`Remove ${props.id}`} onRemove={props.onRemove} />
      </div>
      <RowSub title={launch}>{launch}</RowSub>
    </RowShell>
  );
}

/** Desktop detail: the selected entry's preview. Keyed by id so the
 * two-step confirm state resets per selection. `lastAdd` carries the
 * fresh add's per-tool outcomes for THIS selection (panel-level state,
 * cleared on remove). */
function McpPreview(props: {
  id: string;
  lastAdd?: McpToolRegistration[];
  onRemove: () => Promise<string | undefined>;
}): React.ReactElement {
  const server = useFlux((s) => s.mcpServers.find((x) => x.id === props.id));
  if (!server) return <EmptyState>Server removed.</EmptyState>;
  const launch = connectLine(server);
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border bg-panel p-4">
      <div className="flex items-center gap-2">
        <RowTitle>{server.id}</RowTitle>
        <StateBadge state={server.state} />
        {server.env_keys.map((k) => (
          <Badge key={k} title={`env key ${k} (the value never leaves the server)`}>
            {k}
          </Badge>
        ))}
        {server.header_keys.map((k) => (
          <Badge key={k} title={`header ${k} (the value never leaves the server)`}>
            {k}
          </Badge>
        ))}
        <span className="flex-1" />
        <RemoveControl label={`Remove ${server.id}`} onRemove={props.onRemove} />
      </div>
      <RowSub title={launch}>{launch}</RowSub>
      <ToolChips server={server} />
      {props.lastAdd && <AddResults results={props.lastAdd} />}
      <span className="text-2xs text-faint">Applies live — running conversations pick the tools up when their current round ends.</span>
    </div>
  );
}

const HINT =
  'External tool servers — local child processes or remote Streamable HTTP endpoints — ' +
  'connected at server start and stored in the server database. Env and header values never ' +
  'leave the server after saving (keys only); editing an entry = remove + re-add.';

export function McpPanel(): React.ReactElement {
  const servers = useFlux((s) => s.mcpServers);
  const isMobile = useIsMobile();
  const { selected, choose, markPending, noteRemoved } = useRailSelection(servers, keyOf);

  // The creation-form draft lives at panel level: switching the selection
  // never loses a half-typed entry.
  const [id, setId] = useState('');
  const [kind, setKind] = useState<McpKind>('stdio');
  const [command, setCommand] = useState('');
  const [argsText, setArgsText] = useState('');
  const [envText, setEnvText] = useState('');
  const [urlText, setUrlText] = useState('');
  const [headersText, setHeadersText] = useState('');
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);
  // The fresh add's per-tool outcomes (the debugging surface — a skipped
  // tool's reason rides the ack, never only the server log). Cleared when
  // that entry is removed.
  const [lastAdd, setLastAdd] = useState<{ id: string; results: McpToolRegistration[] } | null>(
    null,
  );

  // The list may be cold — pull on section show.
  useEffect(() => {
    fetchMcpServers();
  }, []);

  const canAdd =
    id.trim() !== '' && !adding && (kind === 'stdio' ? command.trim() !== '' : urlText.trim() !== '');

  const runAdd = () => {
    // The kind-conditional key=value block: env for stdio, headers for
    // http — the same parser, one mental model.
    const kvText = kind === 'stdio' ? envText : headersText;
    const { env: kv, error } = parseEnv(kvText);
    if (error) {
      setAddError(error);
      return;
    }
    setAdding(true);
    setAddError(null);
    const added = id.trim();
    void addMcpServer({
      id,
      kind,
      command,
      args: kind === 'stdio' ? parseLines(argsText) : [],
      env: kind === 'stdio' ? kv : {},
      url: urlText,
      headers: kind === 'http' ? kv : {},
    }).then(
      ({ error: addErr, results }) => {
        setAdding(false);
        if (addErr) {
          setAddError(addErr);
          return;
        }
        // Success: the broadcast refreshes the list (and lands the pending
        // selection) — reset the form.
        markPending(added);
        setLastAdd({ id: added, results });
        setId('');
        setCommand('');
        setArgsText('');
        setEnvText('');
        setUrlText('');
        setHeadersText('');
        const skipped = results.filter((r) => !r.registered).length;
        useFlux
          .getState()
          .pushToast(
            'info',
            skipped > 0
              ? `MCP server "${added}" added — ${results.length - skipped} tools registered, ${skipped} skipped`
              : `MCP server "${added}" added — applied to new rounds`,
          );
      },
    );
  };

  /** Shared by the preview pane and the mobile rows. */
  const remove = (sid: string): Promise<string | undefined> => {
    noteRemoved(sid);
    return removeMcpServer(sid).then((error) => {
      if (!error) {
        setLastAdd((l) => (l?.id === sid ? null : l));
        useFlux.getState().pushToast('info', `MCP server "${sid}" removed — applied to new rounds`);
      }
      return error;
    });
  };

  const selectedServer = selected === 'new' ? undefined : servers.find((s) => keyOf(s) === selected);

  // ── Mobile: the stacked layout (hint, notice, rows, form) ──
  if (isMobile) {
    return (
      <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
        <DialogHint>{HINT}</DialogHint>
        <NoticeBar>
          Changes apply live — running conversations pick the tools up when their current round
          ends; a command that fails to spawn is retried at the next server start.
        </NoticeBar>
        <div className="flex flex-col gap-2">
          <SectionLabel>Registered</SectionLabel>
          {servers.length === 0 ? (
            <EmptyState>No MCP servers registered — add one below.</EmptyState>
          ) : (
            <ul aria-label="MCP servers" className="m-0 flex list-none flex-col gap-2 p-0">
              {servers.map((s) => (
                <McpRow
                  key={s.id}
                  id={s.id}
                  kind={s.kind}
                  command={s.command}
                  args={s.args}
                  url={s.url}
                  envKeys={s.env_keys}
                  headerKeys={s.header_keys}
                  state={s.state}
                  toolNames={s.tool_names}
                  onRemove={() => remove(s.id)}
                />
              ))}
            </ul>
          )}
        </div>
        <McpForm
          id={id}
          kind={kind}
          command={command}
          argsText={argsText}
          envText={envText}
          urlText={urlText}
          headersText={headersText}
          adding={adding}
          canAdd={canAdd}
          addError={addError}
          setId={setId}
          setKind={setKind}
          setCommand={setCommand}
          setArgsText={setArgsText}
          setEnvText={setEnvText}
          setUrlText={setUrlText}
          setHeadersText={setHeadersText}
          onSubmit={runAdd}
        />
      </div>
    );
  }

  // ── Desktop: master-detail ──
  return (
    <MasterDetail
      list={
        <Rail label="MCP servers">
          <NewRailButton label="New server" selected={selected === 'new'} onClick={() => choose('new')} />
          {servers.map((s) => (
            <RailButton
              key={s.id}
              selected={selected === s.id}
              onClick={() => choose(s.id)}
              ariaLabel={`Select server ${s.id}`}
              title={s.id}
              sub={connectLine(s)}
            />
          ))}
          {servers.length === 0 && (
            <li className="px-2 py-1 text-2xs leading-relaxed text-faint">No MCP servers registered.</li>
          )}
        </Rail>
      }
      detail={
        <DetailPane>
          {selectedServer ? (
            <McpPreview
              key={selectedServer.id}
              id={selectedServer.id}
              lastAdd={lastAdd?.id === selectedServer.id ? lastAdd.results : undefined}
              onRemove={() => remove(selectedServer.id)}
            />
          ) : (
            <>
              <DialogHint>{HINT}</DialogHint>
              <NoticeBar>
                Changes apply live — running conversations pick the tools up when their current
                round ends; a command that fails to spawn is retried at the next server start.
              </NoticeBar>
              <McpForm
                id={id}
                kind={kind}
                command={command}
                argsText={argsText}
                envText={envText}
                urlText={urlText}
                headersText={headersText}
                adding={adding}
                canAdd={canAdd}
                addError={addError}
                setId={setId}
                setKind={setKind}
                setCommand={setCommand}
                setArgsText={setArgsText}
                setEnvText={setEnvText}
                setUrlText={setUrlText}
                setHeadersText={setHeadersText}
                onSubmit={runAdd}
              />
            </>
          )}
        </DetailPane>
      }
    />
  );
}
