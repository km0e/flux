-- Consolidated schema (single file, final shape). Unreleased — no legacy
-- databases; schema changes edit this file in place (the established
-- "upgrade = rebuild" decision), while the sqlx migration mechanism stays
-- for a future released version.
--
-- Provider pins are REQUIRED: every chat created through CreateChat
-- persists its (provider, model) pin as keyed `state` rows, so no row
-- ever ships without one (the store maps a NULL — hand-edited rows only —
-- to an explicit spawn-time error, never a fallback).

CREATE TABLE IF NOT EXISTS chats (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT 'New Chat',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- The chat's most recent message-append time — the sidebar's recency
    -- key (a chat's first activity is its creation).
    last_activity_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    -- Fork provenance: when this chat was forked, the SOURCE conversation
    -- and the copied-through message row id. NULL = not a fork.
    forked_from_chat    TEXT REFERENCES chats(id) ON DELETE SET NULL,
    forked_from_message INTEGER
);

CREATE TABLE IF NOT EXISTS messages (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id            TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    role               TEXT NOT NULL,
    content            TEXT NOT NULL DEFAULT '',
    reasoning_content  TEXT,
    tool_call_id       TEXT,
    created_at         TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_messages_chat
    ON messages(chat_id, id);

CREATE TABLE IF NOT EXISTS tool_calls (
    id         TEXT NOT NULL,
    message_id INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    arguments  TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (id, message_id)
);
CREATE INDEX IF NOT EXISTS idx_tool_calls_msg
    ON tool_calls(message_id);

-- Per-chat state (key/value): the persisted provider+model pin pair and
-- the creation's workdir+current_dir pair. FK cascade deletes with the chat.
CREATE TABLE IF NOT EXISTS state (
    chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    key     TEXT NOT NULL,
    value   TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (chat_id, key)
);

-- The provider registry: providers are pure endpoints (id, interface type,
-- url, api_key) with NO model — the model is a required per-chat pin. The
-- registry lives ONLY here (the config file no longer carries providers);
-- the server hydrates its in-memory registry from this table at startup
-- and the UI manages it over the Connect surface (AddProvider /
-- RemoveProvider RPCs).
CREATE TABLE IF NOT EXISTS providers (
    id      TEXT PRIMARY KEY,
    type    TEXT NOT NULL DEFAULT 'openai',
    url     TEXT,
    api_key TEXT
);

-- The LOCAL model registry: user-saved models per provider, each carrying
-- editable request params and (when a models.dev match was found) a
-- metadata snapshot. Two JSON columns with SEPARATED write authority:
-- `params` is client-authoritative (model_save writes it, never meta);
-- `meta` is server-authoritative (written only at import/refresh from
-- models.dev, never clobbered by an edit). API-catalog entries are NOT
-- stored — the catalog stays probe-transient; importing copies a row here.
CREATE TABLE IF NOT EXISTS models (
    provider_id TEXT NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    model_id    TEXT NOT NULL,
    params      TEXT NOT NULL DEFAULT '{}',
    meta        TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (provider_id, model_id)
);

-- MCP servers to launch as child processes. The ONLY home (the server
-- has no config file); the UI manages the rows (AddServer /
-- RemoveServer RPCs) — persist-first + LIVE apply: the manager spawns
-- and registers the tools immediately, and a self-healing supervisor
-- respawns a dead child. args/env are JSON (array / object) — env
-- VALUES are stored here but never leave the server over the wire
-- (secrets parity with api_key).
CREATE TABLE IF NOT EXISTS mcp_servers (
    id      TEXT PRIMARY KEY,
    command TEXT NOT NULL,
    args    TEXT NOT NULL DEFAULT '[]',
    env     TEXT NOT NULL DEFAULT '{}'
);

-- Buffered tool outputs — per-chat persistence for the overflow buffer.
-- Each entry is ANCHORED to the tool call that produced it (keyed by the
-- kernel-assigned call id): the truncated result in the transcript points
-- at the same id, so the reference is self-describing and stable across
-- engine rebuilds AND process restarts. Entries are never overwritten;
-- a chat's transcript only ever grows (there is no archive boundary), so
-- an entry lives exactly as long as its chat (FK cascade) — and a FORK
-- copies the entries of the calls its copied transcript carries, keeping
-- `buf_read` references resolvable in the new chat.
CREATE TABLE IF NOT EXISTS buf_entries (
    chat_id  TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    call_id  TEXT NOT NULL,
    content  TEXT NOT NULL,
    PRIMARY KEY (chat_id, call_id)
);
