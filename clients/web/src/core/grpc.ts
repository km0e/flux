/**
 * grpc.ts — the Connect client: every server conversation rides gRPC-Web
 * from here (typed contracts from the generated `gen/flux/v1`).
 *
 * Layers:
 * - `transport` — the shared gRPC-Web transport (same origin, binary).
 * - Per-family clients + call helpers (fs / providers / models / mcp /
 *   skills): fetch-into-store reads and promise-shaped mutations whose
 *   failures ride the response's inline `error` (D4' — request-scoped
 *   failures are UI data, never a transport status).
 * - The event plane + chat control plane live in `grpc-connection.ts`
 *   (the stream anchors the identity; the control RPCs translate the
 *   ClientMessage vocabulary).
 *
 * Provides: transport, grpc*, call helpers, proto mapping helpers
 * Depends: gen/flux/v1, core/state.ts, core/types.ts, logger.ts
 */

import { createClient } from '@connectrpc/connect';
import { createGrpcWebTransport } from '@connectrpc/connect-web';
import { FileSystemService, FsEntryKind, GitStatus } from '../gen/flux/v1/fs_pb';
import { readStoredSessionId } from './session';
import type { McpState, McpToolRegistration } from './types';
import { ProviderService } from '../gen/flux/v1/providers_pb';
import { ModelService } from '../gen/flux/v1/models_pb';
import { McpService } from '../gen/flux/v1/mcp_pb';
import { SkillService } from '../gen/flux/v1/skills_pb';
import { McpState as ProtoMcpState, SkillSource } from '../gen/flux/v1/common_pb';
import { ChatService } from '../gen/flux/v1/chats_pb';
import { EventService } from '../gen/flux/v1/events_pb';
import type { FsEntry, FsListing, FsContent } from './types';
import { log } from '../logger';
import { useFlux } from './state';
import type { ModelParams, ProviderModelInfo } from './types';

/** gRPC-Web over HTTP/1.1, same origin, binary encoding. */
export const transport = createGrpcWebTransport({
  baseUrl: typeof location === 'undefined' ? '' : location.origin,
  useBinaryFormat: true,
});

export const fsClient = createClient(FileSystemService, transport);
export const providerClient = createClient(ProviderService, transport);
export const modelClient = createClient(ModelService, transport);
export const mcpClient = createClient(McpService, transport);
export const skillClient = createClient(SkillService, transport);
export const chatClient = createClient(ChatService, transport);
export const eventsClient = createClient(EventService, transport);

/** The replaceable client surface (tests swap instances here — an ES
 * module's `export const` binding cannot be re-pointed, an object
 * property can). The connection layer reads through this object. */
export const clients = {
  fs: fsClient,
  providers: providerClient,
  models: modelClient,
  mcp: mcpClient,
  skills: skillClient,
  chat: chatClient,
  events: eventsClient,
};

/** The identity metadata for unary calls: the session token rides
 * `x-flux-session` (the stream anchored it; the lease-gated RPCs resolve
 * it server-side). Absent while detached — those calls fail
 * unauthenticated and the caller's inline error renders it. */
function auth(): { headers: Record<string, string> } {
  const token = readStoredSessionId() ?? '';
  return { headers: token ? { 'x-flux-session': token } : {} };
}

/** Map a proto FsEntryKind into the wire enum string the UI renders. */
export function mapEntryKind(kind: FsEntryKind): 'dir' | 'file' {
  return kind === FsEntryKind.DIR ? 'dir' : 'file';
}

/** Map a proto GitStatus into the wire enum string the UI renders. */
export function mapGitStatus(git: GitStatus): FsEntry['git'] {
  switch (git) {
    case GitStatus.MODIFIED:
      return 'modified';
    case GitStatus.ADDED:
      return 'added';
    case GitStatus.UNTRACKED:
      return 'untracked';
    case GitStatus.CONFLICTED:
      return 'conflicted';
    default:
      return undefined;
  }
}

/** Map the proto response onto the UI's FsListing shape (pure, tested).
 * The parameter is the response's STRUCTURAL shape (works for the runtime
 * Message and its plain form alike). */
export function protoListingToListing(resp: {
  requested: string;
  error?: string;
  path?: string;
  parent?: string;
  entries: Array<{ name: string; kind: FsEntryKind; size?: bigint; git?: GitStatus }>;
}): FsListing {
  return {
    type: 'fs_listing',
    requested: resp.requested,
    error: resp.error ?? undefined,
    path: resp.path || undefined,
    parent: resp.parent || undefined,
    entries: resp.entries.map((e) => ({
      name: e.name,
      kind: mapEntryKind(e.kind),
      // proto uint64 → bigint; listing sizes are far below 2^53.
      size: e.size !== undefined ? Number(e.size) : undefined,
      git: mapGitStatus(e.git ?? GitStatus.UNSPECIFIED),
    })),
  };
}

/** Map the proto FsRead response onto the UI's FsContent shape. */
export function protoContentToContent(resp: {
  requested: string;
  error?: string;
  content?: string;
  truncated?: boolean;
  size?: bigint;
}): FsContent {
  return {
    type: 'fs_content',
    requested: resp.requested,
    error: resp.error ?? undefined,
    content: resp.content ?? undefined,
    truncated: resp.truncated ?? undefined,
    size: resp.size !== undefined ? Number(resp.size) : undefined,
  };
}

/** The outage marker — a request failing while the stream is detached
 * resolves with this instead of a per-call message, so surfaces can
 * suppress their error toasts (outage communication belongs to the
 * connection indicator). Callers test it with services/fs.isDisconnect. */
export const DISCONNECTED_ERROR = 'server disconnected';

function failure(whileDisconnected: boolean, op: string, err: unknown): string {
  if (whileDisconnected) return DISCONNECTED_ERROR;
  log.warn('grpc: ' + op + ' failed', err);
  return op + ' failed';
}

/**
 * List one directory over gRPC-Web. Never rejects — failures (transport
 * or inline) resolve with `error` set so the picker renders them. The
 * browser aborts on its own when the dialog closes; an 8s deadline.
 */
export async function grpcListDir(path?: string): Promise<FsListing> {
  const offline = useFlux.getState().connectionStatus !== 'connected';
  try {
    const resp = await fsClient.fsList({ path: path ?? '' }, { timeoutMs: 8000, ...auth() });
    return protoListingToListing(resp);
  } catch (err) {
    return {
      type: 'fs_listing',
      requested: path ?? '',
      error: failure(offline, 'listing', err),
      entries: [],
    };
  }
}

/**
 * Read a bounded head of one file for the preview pane. Never rejects —
 * failures resolve with `error` set (same inline contract).
 */
export async function grpcReadFile(path: string): Promise<FsContent> {
  const offline = useFlux.getState().connectionStatus !== 'connected';
  try {
    const resp = await fsClient.fsRead({ path }, { timeoutMs: 8000, ...auth() });
    return protoContentToContent(resp);
  } catch (err) {
    return { type: 'fs_content', requested: path, error: failure(offline, 'read', err) };
  }
}

// ── Providers management ───────────────────────────────────────────────────

/** Fetch the registry and publish it into the store. */
export async function grpcFetchProviders(): Promise<void> {
  const resp = await providerClient.listProviders({}, { timeoutMs: 8000, ...auth() });
  useFlux.setState({
    providers: resp.providers.map((p) => ({ id: p.id, url: p.url })),
  });
}

/** Add a provider. Resolves with the inline error, or undefined on
 * success (the fresh list arrives via the `providers` broadcast). */
export async function grpcAddProvider(input: {
  id: string;
  url?: string;
  api_key?: string;
}): Promise<string | undefined> {
  const resp = await providerClient.addProvider(
    {
      id: input.id.trim(),
      protocol: 'openai',
      url: input.url?.trim() || undefined,
      apiKey: input.api_key || undefined,
    },
    { timeoutMs: 8000, ...auth() },
  );
  return resp.error ?? undefined;
}

/** Remove a provider. Resolves with the inline error, or undefined. */
export async function grpcRemoveProvider(id: string): Promise<string | undefined> {
  const resp = await providerClient.removeProvider({ id }, { timeoutMs: 8000, ...auth() });
  return resp.error ?? undefined;
}

/** Probe one provider's upstream catalog. Resolves with the models + the
 * in-band error (undefined = clean probe). */
export async function grpcProbeProvider(
  providerId: string,
): Promise<{ models: ProviderModelInfo[]; error?: string }> {
  const resp = await providerClient.getModels({ provider: providerId }, { timeoutMs: 15000, ...auth() });
  return {
    models: resp.models.map((m) => ({
      id: m.id,
      context_length: m.contextLength !== undefined ? Number(m.contextLength) : undefined,
    })),
    error: resp.error ?? undefined,
  };
}

// ── Models management ──────────────────────────────────────────────────────

/** Fetch the saved-model list and publish it into the store. */
export async function grpcFetchModels(): Promise<void> {
  const resp = await modelClient.listModels({}, { timeoutMs: 8000, ...auth() });
  useFlux.setState({
    savedModels: resp.models.map((m) => ({
      provider: m.provider,
      model: m.model,
      params: JSON.parse(m.paramsJson || '{}'),
      meta: JSON.parse(m.metaJson || '{}'),
    })),
  });
}

export async function grpcSaveModel(
  provider: string,
  model: string,
  params: ModelParams,
): Promise<{ enriched?: boolean; error?: string }> {
  const resp = await modelClient.saveModel(
    { provider, model, paramsJson: JSON.stringify(params) },
    { timeoutMs: 15000, ...auth() },
  );
  return { enriched: resp.enriched || undefined, error: resp.error ?? undefined };
}

export async function grpcRemoveModel(provider: string, model: string): Promise<string | undefined> {
  const resp = await modelClient.removeModel({ provider, model }, { timeoutMs: 8000, ...auth() });
  return resp.error ?? undefined;
}

export async function grpcSyncModels(
  provider?: string,
  model?: string,
): Promise<{ updated?: number; error?: string }> {
  const resp = await modelClient.syncModels(
    { provider: provider ?? undefined, model: model ?? undefined },
    { timeoutMs: 30000, ...auth() },
  );
  return { updated: resp.updated || undefined, error: resp.error ?? undefined };
}

// ── MCP management ─────────────────────────────────────────────────────────

/** Map the wire McpState enum onto the UI-side union. */
export function mcpStateOf(state: ProtoMcpState): McpState {
  switch (state) {
    case ProtoMcpState.RUNNING:
      return 'running';
    case ProtoMcpState.BACKOFF:
      return 'backoff';
    case ProtoMcpState.OFFLINE:
      return 'offline';
    default:
      return 'unspecified';
  }
}

/** Fetch the MCP launch list and publish it into the store. */
export async function grpcFetchMcpServers(): Promise<void> {
  const resp = await mcpClient.listServers({}, { timeoutMs: 8000, ...auth() });
  useFlux.setState({
    mcpServers: resp.servers.map((s) => ({
      id: s.id,
      command: s.command,
      args: s.args,
      env_keys: s.envKeys,
      state: mcpStateOf(s.state),
      tool_names: s.toolNames,
    })),
  });
}

export async function grpcAddMcpServer(input: {
  id: string;
  command: string;
  args: string[];
  env: Record<string, string>;
}): Promise<{ error?: string; results: McpToolRegistration[] }> {
  const resp = await mcpClient.addServer(
    {
      id: input.id.trim(),
      command: input.command.trim(),
      args: input.args,
      env: input.env,
    },
    { timeoutMs: 15000, ...auth() },
  );
  return {
    error: resp.error ?? undefined,
    results: resp.results.map((r) => ({
      name: r.name,
      registered: r.registered,
      reason: r.reason ?? undefined,
    })),
  };
}

export async function grpcRemoveMcpServer(id: string): Promise<string | undefined> {
  const resp = await mcpClient.removeServer({ id }, { timeoutMs: 8000, ...auth() });
  return resp.error ?? undefined;
}

// ── Skills management ──────────────────────────────────────────────────────

/** Fetch the installed skills and publish them into the store. */
export async function grpcFetchSkills(chatId?: string): Promise<void> {
  const resp = await skillClient.listSkills({ chatId: chatId ?? undefined }, { timeoutMs: 8000, ...auth() });
  useFlux.setState({
    skills: resp.skills.map((s) => ({
      name: s.name,
      description: s.description,
      source: s.source === SkillSource.PROJECT ? 'project' : 'global',
      removable: s.removable,
    })),
  });
}

export async function grpcAddSkill(input: {
  path?: string;
  url?: string;
  subpath?: string;
}): Promise<string | undefined> {
  const path = input.path?.trim();
  const url = input.url?.trim();
  const resp = await skillClient.addSkill(
    {
      source: path
        ? { case: 'path', value: path }
        : url
          ? { case: 'url', value: url }
          : { case: undefined },
      subpath: input.subpath?.trim() || undefined,
    },
    { timeoutMs: 30000, ...auth() },
  );
  return resp.error ?? undefined;
}

export async function grpcRemoveSkill(name: string): Promise<string | undefined> {
  const resp = await skillClient.removeSkill({ name }, { timeoutMs: 15000, ...auth() });
  return resp.error ?? undefined;
}
