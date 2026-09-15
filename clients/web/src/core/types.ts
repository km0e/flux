/**
 * types.ts — the handler-table's wire vocabulary.
 *
 * ServerMessage and ClientMessage are discriminated unions matching the
 * frames core/grpc-connection.ts translates out of / onto the flux.v1
 * Connect surface. The proto is the single contract source; this file is
 * the UI-side adapter vocabulary the handler table consumes.
 *
 * Provides: All protocol type definitions
 */
// ── Server → Client Messages ──

/** Structured error codes — mirrors the generated flux.v1 ErrorCode enum
 * (the proto is the contract; the translation maps it 1:1). */
export type ErrorCode =
  | 'provider_connection'
  | 'tool_execution'
  | 'invalid_arguments'
  | 'chat_busy'
  | 'chat_not_found'
  | 'stream_crashed'
  | 'stream_gap'
  | 'invalid_request'
  | 'internal';

export interface ChatInfo {
  chat_id: string;
  name: string;
  created_at: string;
  /** Most recent message-append time — the sidebar's recency key. */
  last_activity_at: string;
  /** Another window holds the lease (non-null) — the sidebar shows "In use". */
  active: boolean;
  /** The chat's working directory (the tool sandbox boundary; shown as a cwd line). */
  workdir: string;
  /** The chat's pinned provider registry id. */
  provider: string;
  /** The chat's resolved model string. */
  model: string;
  /** Fork provenance — the SOURCE conversation (`undefined` = not a fork). */
  forked_from_chat_id?: string;
}

/** One registered provider (reply to `provider_list`). Entries are pure
 * endpoints — the summary carries the id and the EFFECTIVE base url (the
 * management UI displays it) but NEVER the api_key. The model is a
 * per-chat pin, so the summary carries none. */
export interface ProviderSummary {
  id: string;
  url: string;
}

/** One probed upstream model (reply to `provider_models`). `context_length`
 * rides only when the gateway exposed it (vLLM/Groq/OpenRouter extras) —
 * the standard OpenAI payload has none, so it's usually absent. */
export interface ProviderModelInfo {
  id: string;
  context_length?: number;
}

/** Editable model params (the `params` JSON of a saved-model row). All
 * optional — absent = not sent, the upstream default applies.
 * `context_length` is informational only (ContextMeter); the rest ride the
 * chat-completions request. */
export interface ModelParams {
  context_length?: number;
  max_tokens?: number;
  temperature?: number;
  top_p?: number;
  [key: string]: unknown;
}

/** One LOCAL saved model (reply to `model_list` / the `models`
 * broadcast). `params` is the editable knob JSON; `meta` is the server's
 * models.dev metadata snapshot ({} when the model matched nothing) —
 * display-only, never client-writable. */
export interface SavedModelInfo {
  provider: string;
  model: string;
  params: ModelParams;
  meta: {
    name?: string;
    reasoning?: boolean;
    tool_call?: boolean;
    temperature?: boolean;
    attachment?: boolean;
    context_length?: number;
    max_output?: number;
    knowledge?: string;
    release_date?: string;
    cost?: {
      input?: number;
      output?: number;
      cache_read?: number;
      cache_write?: number;
    };
    source?: string;
    models_dev?: { provider: string; model: string };
    [key: string]: unknown;
  };
}

/** One registered MCP server (reply to `mcp_list`). Carries the launch
 * triple EXCEPT the env values — only the KEYS leave the server (secrets
 * parity with the provider api_key). Mutations apply LIVE: persist-first,
 * then the manager spawns/registers and the chats rebuild at the gate. */
/** Live state of one MCP server (the self-healing supervisor). */
export type McpState = 'unspecified' | 'running' | 'backoff' | 'offline';

/** One entry of the current round's artifacts (F-11). `file` = a path the
 * round created/modified (extracted from the tool's declared path
 * argument); `tool` = a shell command or an MCP invocation (no path to
 * extract — the label IS the entry). `callId` is the LATEST touch's tool
 * call — the jump anchor into the message stream. */
export interface RoundArtifact {
  kind: 'file' | 'tool';
  callId: string;
  /** file: the path; tool: the display label (command / tool name). */
  target: string;
  /** file only: how the tool touched the path. */
  change?: 'write' | 'edit';
  /** tool only: where the invocation came from. */
  source?: 'shell' | 'mcp';
}

/** How flux connects to one registered MCP server. */
export type McpKind = 'stdio' | 'http';

export interface McpServerSummary {
  id: string;
  kind: McpKind;
  /** Executable to run (stdio rows; empty for http). */
  command: string;
  args: string[];
  env_keys: string[];
  /** The Streamable HTTP endpoint (http rows; empty for stdio). */
  url: string;
  /** Header NAMES for the HTTP transport — values never leave the
   * server. */
  header_keys: string[];
  state: McpState;
  /** Tool names the live session registered — empty when no live
   * session (distinguish via state). */
  tool_names: string[];
}

/** One tool's registration outcome (the add ack's per-tool detail). */
export interface McpToolRegistration {
  name: string;
  registered: boolean;
  reason?: string;
}

/** One forwarded MCP server log notification (the bell menu's ring). */
export interface McpNoticeEntry {
  id: number;
  server_id: string;
  level: string;
  message: string;
  /** Epoch ms — the bell renders relative time. */
  at: number;
}

/** One installed skill (reply to `skills_list`, and the broadcast after a
 * successful add/remove). Global entries are user-installed and removable;
 * project entries live in the chat workdir's repository (read-only here). */
export interface SkillSummary {
  name: string;
  description: string;
  source: 'global' | 'project';
  removable: boolean;
}

// ── Filesystem browser (workdir picker + explorer) ──
/** Git working-tree status of a listed entry (absent = clean / not a repo).
 * Directories aggregate their descendants — the strongest signal wins. */
export type GitEntryStatus = 'modified' | 'added' | 'untracked' | 'conflicted';

/** One directory-listing entry — dirs navigable, files previewable. */
export interface FsEntry {
  name: string;
  kind: 'dir' | 'file';
  size?: number;
  git?: GitEntryStatus;
}

export type FsListing = {
  type: 'fs_listing';
  /** Echoes the request's path ("" = server default start dir). */
  requested: string;
  error?: string;
  /** Canonicalized directory actually listed. */
  path?: string;
  parent?: string;
  entries: FsEntry[];
};

export type FsContent = {
  type: 'fs_content';
  requested: string;
  error?: string;
  content?: string;
  truncated?: boolean;
  /** Full file size in bytes (drives the truncated hint). */
  size?: number;
};

export type ServerMessage =
  | { type: 'ready'; session_id: string }
  | { type: 'text_delta'; chat_id: string; delta: string }
  | { type: 'reasoning_delta'; chat_id: string; delta: string }
  | { type: 'error'; chat_id?: string; code: ErrorCode; message: string }
  | {
      type: 'usage';
      chat_id: string;
      prompt_tokens: number;
      completion_tokens: number;
      cached_tokens: number;
    }
  | { type: 'stream_end'; chat_id: string; finish_reason?: string }
  | { type: 'stream_cancelled'; chat_id: string }
  | { type: 'tool_start'; chat_id: string; id: string; name: string; arguments: string }
  /** A tool call the model is still FORMING (identity + optional argument
   * fragment), sent BEFORE its tool_start. Ephemeral: never in history; a
   * preview with no tool_start by round end is voided client-side. */
  | {
      type: 'tool_preview';
      chat_id: string;
      id: string;
      name?: string;
      arguments_delta?: string;
    }
  | { type: 'tool_result'; chat_id: string; id: string; result: string }
  | {
      type: 'question_required';
      chat_id: string;
      id: string;
      question: { text: string; options?: string[] };
    }
  | { type: 'chats'; chats: ChatInfo[] }
  | { type: 'chat_created'; chat: ChatInfo }
  /** Reply to `provider_list` (and the broadcast after an add/remove):
   * the registered providers (id + effective url, never the api_key). */
  | { type: 'providers'; providers: ProviderSummary[] }
  /** Reply to `provider_add` / `provider_remove`: failures ride the inline
   * `error` (request-scoped, never the global error channel); a success is
   * followed by the `providers` broadcast. */
  | { type: 'provider_added'; id: string; error?: string }
  | { type: 'provider_removed'; id: string; error?: string }
  /** Reply to `mcp_list` (and the broadcast after an add/remove): the
   * registered MCP servers (env values redacted — keys only). */
  | { type: 'mcp_servers'; servers: McpServerSummary[] }
  /** One rate-limited MCP server log notification (session-level,
   * fire-and-forget — not part of the R2 snapshot reconciliation). The
   * level is the MCP spec's lowercase string, passed through verbatim. */
  | { type: 'mcp_notice'; server_id: string; level: string; message: string }
  /** Reply to `mcp_add` / `mcp_remove`: failures ride the inline `error`
   * while the row stays; a success is followed by the `mcp_servers`
   * broadcast, and the change is already LIVE (spawned + registered,
   * matching chats rebuilt at the machine gate). */
  | { type: 'mcp_added'; id: string; error?: string }
  | { type: 'mcp_removed'; id: string; error?: string }
  /** Reply to `skills_list` (and the broadcast after a successful
   * skill_add / skill_remove): the installed skills, sorted by name. */
  | { type: 'skills'; skills: SkillSummary[] }
  /** Reply to `skill_add`: success carries the installed name; every
   * failure (both/neither source, invalid skill dir, git failure, name
   * collision) rides `error` inline. Immediate effect — no restart. */
  | { type: 'skill_added'; name?: string; error?: string }
  /** Reply to `skill_remove`: an unknown (or project-local) name rides
   * `error`. */
  | { type: 'skill_removed'; name?: string; error?: string }
  /** Reply to `provider_models`: the upstream's catalog; probe failures
   * ride `error` (never the global error channel). */
  | { type: 'provider_models'; provider: string; models: ProviderModelInfo[]; error?: string }
  /** Reply to `model_list` (and the broadcast after a successful
   * save/remove/sync): the LOCAL saved models, sorted by (provider,
   * model). */
  | { type: 'models'; models: SavedModelInfo[] }
  /** Reply to `model_save`: failures (unknown provider, invalid params)
   * ride the inline `error`; `enriched` marks a create whose metadata
   * was auto-filled from models.dev. */
  | { type: 'model_saved'; provider: string; model: string; enriched?: boolean; error?: string }
  /** Reply to `model_remove`: an unknown row rides `error`. */
  | { type: 'model_removed'; provider: string; model: string; error?: string }
  /** Reply to `model_sync`: `updated` counts rows whose metadata actually
   * changed; a models.dev fetch failure rides `error` (best-effort). */
  | { type: 'model_synced'; provider?: string; updated: number; error?: string }
  /** The conversation's provider was hot-swapped — subsequent rounds run on
   * the new provider/model with the context rebuilt over the live history. */
  | { type: 'provider_switched'; chat_id: string; provider: string; model: string }
  | { type: 'chat_history'; chat_id: string; messages: HistoryMessage[] }
  | { type: 'chat_state'; chat_id: string; state: 'idle' | 'streaming' }
  /** A user message just persisted with this store row id (announced at
   * turn acceptance). The sender's client matches `content` against its
   * own un-id'd live user bubbles and attaches the fork affordance — the
   * id is the ForkChatRequest.fork_point those bubbles were missing. */
  | { type: 'message_persisted'; chat_id: string; id: number; content: string }
  | FsListing
  | FsContent
  /** The authoritative identity (adopted from the stored token or freshly
   * minted) + the chats the identity still holds. Synthesized from the
   * Subscribe stream's `ready` element — the resume handshake collapsed
   * into the stream open. */
  | { type: 'session_resumed'; session_id: string; leases: string[] };

// ── Client → Server Messages ──

export type ClientMessage =
  | {
      type: 'chat_create';
      name: string;
      workdir: string;
      /** Pin the chat to a registered provider — REQUIRED (no server
       * default; an omitted field is a protocol error). */
      provider: string;
      /** Pin the model — REQUIRED (providers carry no default). */
      model: string;
    }
  | {
      type: 'chat';
      chat_id: string;
      message: string;
      /** R1 interrupt-send: the server fuses "cancel the live round" +
       * "submit this message" into one operation (both ride the kernel's
       * FIFO back-to-back), so the message cannot be lost to the cancel
       * clearing the queue. */
      interrupt?: boolean;
    }
  /** Fork a conversation from a message: a NEW chat holding a copy of
   * the source transcript up to but EXCLUDING `fork_point` (a USER
   * message row of that chat — the redo turn; the client prefills the
   * fork's composer with its content, and it re-enters the fork only
   * when re-sent). The source is untouched; the ack carries the new
   * chat and the fresh chats broadcast follows. */
  | { type: 'fork'; chat_id: string; fork_point: number }
  | { type: 'chat_open'; chat_id: string }
  // The operator open path is a single message: claim = history snapshot +
  // subscription + lease; open is viewer-only (the chat_busy fallback)
  // and stream_gap resubscription.
  | { type: 'chat_claim'; chat_id: string }
  | { type: 'chat_close'; chat_id: string }
  | { type: 'question_response'; chat_id: string; id: string; answer: string }
  | { type: 'chat_list' }
  | { type: 'chat_delete'; chat_id: string }
  | { type: 'chat_rename'; chat_id: string; name: string }
  | { type: 'cancel'; chat_id: string }
  /** Hot-swap the conversation's provider (lease holder only). The swap
   * applies at the round boundary; `provider_switched` announces it.
   * `model` is REQUIRED (same rule as chat_create — no defaults exist). */
  | { type: 'chat_provider'; chat_id: string; provider: string; model: string }
  | { type: 'provider_list' }
  | { type: 'provider_models'; provider: string }
  /** Register/remove a provider (the registry lives in the server
   * database; replies are provider_added/provider_removed with inline
   * errors). `protocol` is optional — only "openai" is supported and the
   * server defaults to it. Saving never validates connectivity; a bad
   * endpoint surfaces at the next round / model probe. */
  | { type: 'provider_add'; id: string; protocol?: string; url?: string; api_key?: string }
  | { type: 'provider_remove'; id: string }
  /** LOCAL saved-model registry: create/edit share `model_save` (upsert;
   * the import flow sends creates and the server auto-fills metadata from
   * models.dev). Replies are model_saved/model_removed/model_synced with
   * inline errors; a success is followed by the `models` broadcast.
   * `meta` is never client-writable — only the server's import/refresh
   * path writes it. */
  | { type: 'model_list' }
  | { type: 'model_save'; provider: string; model: string; params?: ModelParams }
  | { type: 'model_remove'; provider: string; model: string }
  /** Re-sync saved models against models.dev (metadata only). Scope:
   * both absent = every saved model; `provider` alone = that provider's
   * models; both = one model. */
  | { type: 'model_sync'; provider?: string; model?: string }
  /** Register/remove an MCP server (the launch list lives in the server
   * database; replies are mcp_added/mcp_removed with inline errors).
   * Applied live: persist-first, then spawn + register + engine rebuild. */
  | { type: 'mcp_list' }
  | { type: 'mcp_add'; id: string; command: string; args?: string[]; env?: Record<string, string> }
  | { type: 'mcp_remove'; id: string }
  /** List the installed skills for the Skills dialog: the global skills
   * dir plus — with a chat id — that chat's workdir-local (project)
   * skills. Reply: `skills`. */
  | { type: 'skills_list'; chat_id?: string }
  /** Install a skill into the global skills dir — exactly one source: a
   * local directory path OR a git URL (optional subpath inside a
   * multi-skill repo). Immediately effective (no restart). Reply:
   * `skill_added`. */
  | { type: 'skill_add'; path?: string; url?: string; subpath?: string }
  /** Remove a skill from the GLOBAL skills dir by name. Reply:
   * `skill_removed`. */
  | { type: 'skill_remove'; name: string }
  // Filesystem browsing for the workdir picker (any readable
  // directory is a valid chat workdir; the server replies with matching
  // fs_listing / fs_content frames).
  | { type: 'fs_list'; path?: string }
  | { type: 'fs_read'; path: string };

// ── Domain types ──

export interface UsageInfo {
  prompt_tokens: number;
  completion_tokens: number;
  cached_tokens: number;
}

// ── History types ──

export interface HistoryToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface HistoryMessage {
  /** The store row id — the fork point the affordance buttons carry.
   * Absent on messages assembled outside persistence. */
  id?: number;
  role: string;
  content: string;
  reasoning_content?: string;
  /** Absent on the wire when empty — the server skips serializing empty vecs. */
  tool_calls?: HistoryToolCall[];
  tool_call_id?: string;
}
