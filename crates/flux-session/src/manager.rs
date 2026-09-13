//! ServerState — global configuration and ChatManager state.
//!
//! Holds shared configuration and the ChatManager state (in-memory chat
//! cache + session registry). The cache is populated from the store at
//! startup and kept in sync on every mutation. `list_chats` reads from
//! memory — no DB query needed.
//!
//! Lease/subscription bookkeeping and broadcast live in `ops.rs`; the
//! per-chat router (fanout, slow-viewer gap handling, parked prompts)
//! lives in `router.rs`.

use crate::identity::SessionRef;
use crate::router::RouterHandle;
use flux_core::{ChatStateKind, Provider, ToolRegistry};
use flux_store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::RwLock;

/// How long a disconnected session's identity (and its leases) survive,
/// waiting for the client's Subscribe stream to re-open and adopt the
/// token. Covers a page refresh comfortably; a genuinely closed tab is
/// reaped after this.
pub(crate) const SESSION_GRACE: std::time::Duration = std::time::Duration::from_secs(30);

type ChatId = String;

/// One-shot hydration closure: resolves a persisted provider pin (id +
/// the chat's explicitly persisted model) into an instance. Used once
/// inside [`ServerState::new`], never stored — no resident
/// provider-management surface grows back into the chat layer.
pub(crate) type PinLookup<'a> = &'a (dyn Fn(&str, &str) -> Option<Arc<dyn Provider>> + 'a);

/// In-memory cache entry for one chat.
pub(crate) struct CachedChat {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: String,
    /// Most recent message-append time — the wire recency key.
    pub(crate) last_activity_at: String,
    /// The chat's working directory (sandbox boundary; UI display).
    pub(crate) workdir: String,
    /// The chat's pinned provider registry id (resolved at creation; the
    /// router updates it when a hot swap lands).
    pub(crate) provider_id: String,
    /// The chat's resolved model string (the router updates it on swap).
    pub(crate) model: String,
    /// Fork provenance — the SOURCE conversation when this chat was forked
    /// (`None` = not a fork). Wire metadata only; no runtime behavior.
    pub(crate) forked_from_chat: Option<String>,
    /// The chat's resolved provider INSTANCE (model-pinned). `None` = the
    /// persisted pin no longer resolves (registry changed across restarts)
    /// — the task spawn then FAILS with an error naming the dead pin
    /// (recovery: a SwitchProvider swap). Kept next to the id/model
    /// strings so a respawn rebuilds its connection on the SAME provider.
    pub(crate) provider: Option<Arc<dyn Provider>>,
    /// The chat's runtime task, if any. Spawned lazily on the first message
    /// (lifecycle::ensure_task); a terminated task is replaced lazily on
    /// the next send.
    pub(crate) task: Option<crate::lifecycle::ChatTask>,
    /// Session holding the lease, if any. `info().active` mirrors this.
    /// The identity handle is opaque — holder checks compare handles, the
    /// resume token never reaches chat-layer logic.
    pub(crate) lease: Option<SessionRef>,
    /// Per-chat monotonic event sequence (R2): every content element the
    /// router fans out (and every claim/open snapshot) carries the next
    /// value, so a client reconciling a snapshot against the live stream
    /// can order elements. Session-level broadcasts carry 0.
    pub(crate) seq: std::sync::atomic::AtomicU64,
    /// Subscribed identities — the router fans out directly to the sink
    /// each identity carries (single drop surface: the sink's content
    /// queue; gap notices go via the sink's control channel). Keyed by the
    /// identity's internal serial — the router's drop-mark set uses the
    /// same key.
    pub(crate) viewers: HashMap<u64, SessionRef>,
    /// The chat's router handle (fanout + slow-viewer gap + parked question).
    pub(crate) router: RouterHandle,
    /// Pending-question registry shared between the per-chat `question`
    /// tool and the ops layer's `question_response`.
    pub(crate) questions: Arc<flux_chat::question::QuestionBoard>,
    /// Recently accepted send idempotency keys (client_msg_id), FIFO-capped.
    /// A resend carrying an already-accepted key is absorbed as a duplicate
    /// instead of enqueueing a second turn. Lives on the shell, so engine
    /// rebuilds and respawns keep the window.
    pub(crate) recent_msg_ids: std::collections::VecDeque<String>,
}

impl CachedChat {
    /// R2: the chat's next event sequence number. `consume = true` for the
    /// fanout (every viewer sees the same number for the same element —
    /// the router stamps once per element); `false` peeks the CURRENT
    /// value for the claim/open snapshots. The snapshot's value is the
    /// counter the NEXT consumed element will also carry (fetch_add's old
    /// value), so a client drops elements STRICTLY BELOW the snapshot's
    /// value: everything below predates the snapshot, the equal one is the
    /// first live element after it.
    pub(crate) fn next_seq(&self, consume: bool) -> u64 {
        if consume {
            self.seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        } else {
            self.seq.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Owned wire snapshot of this chat's metadata.
    pub fn info(&self) -> ChatInfoOwned {
        ChatInfoOwned {
            chat_id: self.id.clone(),
            name: self.name.clone(),
            created_at: self.created_at.clone(),
            last_activity_at: self.last_activity_at.clone(),
            // Wire `active` means a lease is held.
            active: self.lease.is_some(),
            workdir: self.workdir.clone(),
            provider: self.provider_id.clone(),
            model: self.model.clone(),
            forked_from_chat: self.forked_from_chat.clone(),
        }
    }

    /// Whether the chat's lease is held by `session`. Handle identity —
    /// `ptr_eq`, not token compare: the resume token never reaches
    /// lease/viewer logic.
    pub(crate) fn lease_held_by(&self, session: &SessionRef) -> bool {
        self.lease.as_ref().is_some_and(|h| Arc::ptr_eq(h, session))
    }

    /// The chat's live runtime task handle, if any. A terminated (done)
    /// task is exactly as stale as a missing one — the next send lazily
    /// replaces it.
    pub(crate) fn live_task(&self) -> Option<&flux_chat::handle::ChatHandle> {
        self.task
            .as_ref()
            .map(|t| &t.handle)
            .filter(|h| !h.is_done())
    }

    /// Ensure the identity is a viewer of the chat: register it (with the
    /// sink it carries) so the router can fan out directly. Idempotent.
    /// Every `SessionRef` is a valid receiver by construction — a detached
    /// identity's sends fail gracefully (same path as a stalled viewer).
    pub(crate) fn ensure_viewer(&mut self, session: &SessionRef) {
        self.viewers
            .entry(session.sn())
            .or_insert_with(|| session.clone());
    }
}

/// Owned snapshot of a chat's metadata. Callers collect these under the
/// cache read guard, then drop the guard BEFORE any DB I/O or network send —
/// a slow client must never hold the cache lock open.
#[derive(Debug, Clone)]
pub struct ChatInfoOwned {
    pub chat_id: String,
    pub name: String,
    pub created_at: String,
    /// Most recent message-append time — the wire recency key.
    pub last_activity_at: String,
    /// Whether a session holds the lease — the wire `active` flag.
    pub active: bool,
    /// The chat's working directory.
    pub workdir: String,
    /// The chat's pinned provider registry id.
    pub provider: String,
    /// The chat's resolved model string.
    pub model: String,
    /// Fork provenance — the SOURCE conversation (`None` = not a fork).
    pub forked_from_chat: Option<String>,
}

impl From<&ChatInfoOwned> for flux_proto::flux::v1::ChatInfo {
    fn from(i: &ChatInfoOwned) -> Self {
        Self {
            chat_id: i.chat_id.clone(),
            name: i.name.clone(),
            created_at: i.created_at.clone(),
            last_activity_at: i.last_activity_at.clone(),
            // Wire `active` means a lease is held.
            active: i.active,
            workdir: i.workdir.clone(),
            provider: i.provider.clone(),
            model: i.model.clone(),
            forked_from_chat_id: i.forked_from_chat.clone(),
        }
    }
}

pub struct ChatInfoGuard<'a> {
    pub(crate) guard: tokio::sync::RwLockReadGuard<'a, HashMap<ChatId, CachedChat>>,
}

impl ChatInfoGuard<'_> {
    /// Collect owned snapshots — the guard is then free to drop before any
    /// I/O or send.
    pub fn chats_owned(&self) -> Vec<ChatInfoOwned> {
        self.guard.values().map(|c| c.info()).collect()
    }

    /// Owned wire snapshot of one cached chat.
    pub fn info(&self, chat_id: &str) -> Option<ChatInfoOwned> {
        self.guard.get(chat_id).map(|c| c.info())
    }
}

/// ChatManager state — the chat cache and the identity registry.
///
/// Lock order invariant: ALWAYS acquire `chats` before `identities` (ops
/// and the router fanout both obey). Violating this order can deadlock the
/// manager. Identity interior state (connection sink, detached marker)
/// uses std mutexes and is never held across an await.
pub(crate) struct ManagerState {
    pub(crate) chats: RwLock<HashMap<ChatId, CachedChat>>,
    /// Every known identity, live AND detached (a resumed identity returns
    /// to live without leaving this map). Keyed by the resume token — only
    /// the handshake (token→identity lookup) and the reaper touch it; all
    /// lease/viewer/routing paths hold the `SessionRef` directly.
    pub(crate) identities: RwLock<HashMap<String, SessionRef>>,
    /// The grace window in milliseconds (test-injectable via the atomic —
    /// router tasks hold strong Arcs of this state, so `Arc::get_mut` is
    /// never available; production default = [`SESSION_GRACE`]).
    pub(crate) grace_ms: std::sync::atomic::AtomicU64,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self {
            chats: RwLock::new(HashMap::new()),
            identities: RwLock::new(HashMap::new()),
            grace_ms: AtomicU64::new(SESSION_GRACE.as_millis() as u64),
        }
    }
}

impl ManagerState {
    /// The grace window as a duration.
    pub(crate) fn grace(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.grace_ms.load(std::sync::atomic::Ordering::Relaxed))
    }
}

impl ServerState {
    /// Override the resume grace window. Test/ops seam: the production
    /// default is [`SESSION_GRACE`]; the stream-close→reaper timing tests
    /// shorten it to observe the teardown within the test clock.
    pub fn set_grace(&self, grace: std::time::Duration) {
        self.manager.grace_ms.store(
            grace.as_millis() as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

pub struct ServerState {
    /// Runtime resource fields — crate-internal (lifecycle.rs needs them);
    /// `store` stays `pub` for the transport's history load. There is NO
    /// server default provider: every chat spawns on its own resolved pin,
    /// and an unresolvable pin is an explicit spawn error. Provider
    /// management itself (the registry: selection, instance building,
    /// model probes) lives in flux-server — the chat layer only consumes
    /// resolved instances.
    /// The agent preamble (a plain config field, like `initial_state` —
    /// not provider management). Every connection begins over it.
    pub(crate) system_prompt: Arc<str>,
    pub(crate) tool_registry: Arc<ToolRegistry>,
    pub store: Arc<Store>,
    /// Initial state key → description (for per-chat StateManager construction).
    pub(crate) initial_state: Arc<HashMap<String, String>>,
    /// ChatManager state: chat cache (loaded from DB at startup) + session
    /// registry. Lease/subscription ops and broadcasts live in `ops.rs`.
    pub(crate) manager: Arc<ManagerState>,
}

impl ServerState {
    /// Graceful-shutdown drain: ask every live chat task to cancel its
    /// in-flight round, wait bounded for the rounds to land on the
    /// machine's round boundary (Idle), then force-abort stragglers.
    /// Called ONCE by the transport after the listener has stopped
    /// accepting requests — SIGTERM/SIGINT lands here instead of killing
    /// mid-flight. `send_cancel` is absorbed in Idle (a no-op for quiet
    /// chats); a cancelled round commits its transcript atomically at the
    /// round boundary (the machine's invariant), so whatever completed is
    /// on disk and the next rebirth resumes clean. Note the wait target
    /// is the ROUND boundary, not task termination — a cancelled round
    /// leaves the task alive-but-idle, which is exactly the quiescent
    /// state shutdown wants (the process exits right after).
    pub async fn drain(&self, timeout: std::time::Duration) {
        // ChatHandle is a cheap Clone (senders + flags) — copied OUT of
        // the read guard so the bounded wait below never holds the lock
        // (ops and the router fanout keep needing it).
        let handles: Vec<flux_chat::handle::ChatHandle> = {
            let chats = self.manager.chats.read().await;
            chats
                .values()
                .filter_map(|c| c.live_task().cloned())
                .collect()
        };
        let settled = |h: &flux_chat::handle::ChatHandle| h.active_state() == ChatStateKind::Idle;
        if handles.is_empty() {
            return;
        }
        tracing::info!(live = handles.len(), "drain: cancelling live rounds");
        for handle in &handles {
            handle.send_cancel();
        }
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if handles.iter().all(settled) {
                tracing::info!(live = handles.len(), "drain: all rounds settled");
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        // Stragglers (a flight stuck past its grace, a wedged stream):
        // abort — the same teardown a crash gets, minus the ambiguity
        // about what is on disk (every completed commit is transactional).
        for handle in &handles {
            if !settled(handle) {
                handle.shutdown();
            }
        }
        tracing::warn!(
            live = handles.len(),
            "drain: timeout hit, stragglers aborted"
        );
    }

    /// Build the server state, loading the chat cache from the store.
    /// Fail-fast on a store error: a DB that cannot list chats would
    /// otherwise present every conversation as deleted.
    ///
    /// `lookup` is a one-shot hydration closure: at startup each cached
    /// chat's persisted provider pin resolves to an instance here (the
    /// closure captures the server's registry; used once, never stored —
    /// no resident provider-management surface grows back into the chat
    /// layer). A miss leaves `CachedChat.provider` empty — the chat then
    /// fails EXPLICITLY at spawn (naming the dead pin) instead of silently
    /// running on some other provider; recovery is a SwitchProvider swap.
    pub async fn new(
        system_prompt: Arc<str>,
        tool_registry: Arc<ToolRegistry>,
        store: Arc<Store>,
        initial_state: HashMap<String, String>,
        lookup: PinLookup<'_>,
    ) -> anyhow::Result<Self> {
        let manager = Arc::new(ManagerState::default());
        // Populate the chat cache from the store; each chat gets a fresh
        // router task (idle until events flow through it).
        let mut loaded: HashMap<ChatId, CachedChat> = HashMap::new();
        for s in store.list_chats().await? {
            // Hydrate the pin into an instance. A pin is REQUIRED at
            // creation, so a miss here means the registry changed across
            // restarts — the chat spawns nowhere until the operator swaps
            // providers (the spawn path reports the dead pin explicitly).
            let provider = match (s.provider.as_deref(), s.model.as_deref()) {
                // Both halves are pinned explicitly at creation; a missing
                // half (legacy row) leaves the instance empty — the spawn
                // path reports the dead pin explicitly.
                (Some(id), Some(model)) if !id.is_empty() && !model.is_empty() => lookup(id, model),
                _ => None,
            };
            loaded.insert(
                s.chat_id.clone(),
                CachedChat {
                    seq: std::sync::atomic::AtomicU64::new(0),
                    id: s.chat_id.clone(),
                    name: s.name,
                    created_at: s.created_at.clone(),
                    last_activity_at: s.last_activity_at,
                    workdir: s.workdir.unwrap_or_default(),
                    forked_from_chat: s.forked_from_chat,
                    provider_id: s.provider.unwrap_or_default(),
                    model: s.model.unwrap_or_default(),
                    provider,
                    task: None,
                    lease: None,
                    viewers: HashMap::new(),
                    questions: Arc::new(flux_chat::question::QuestionBoard::new()),
                    router: RouterHandle::spawn(s.chat_id, Arc::clone(&manager)),
                    recent_msg_ids: std::collections::VecDeque::new(),
                },
            );
        }
        manager.chats.write().await.extend(loaded);
        Ok(Self {
            system_prompt,
            tool_registry,
            store,
            initial_state: Arc::new(initial_state),
            manager,
        })
    }

    /// Lock the cache for reading. Callers collect [`ChatInfoOwned`]
    /// snapshots and drop the guard before any I/O or send.
    pub async fn chat_info_guard(&self) -> ChatInfoGuard<'_> {
        ChatInfoGuard {
            guard: self.manager.chats.read().await,
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

pub(crate) fn new_chat_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{sess, test_state};
    use tempfile::tempdir;

    #[tokio::test]
    async fn server_state_new_fails_fast_when_chat_list_load_fails() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        // Break the chats table — list_chats fails, and ServerState::new
        // must fail fast instead of presenting an empty chat list (the
        // "all chats deleted" illusion).
        sqlx::query("DROP TABLE chats")
            .execute(&store.pool)
            .await
            .expect("drop chats table");
        let err = match ServerState::new(
            Arc::from(""),
            Arc::new(ToolRegistry::default()),
            store,
            HashMap::new(),
            &|_, _| None,
        )
        .await
        {
            Ok(_) => panic!("ServerState::new must fail when the chat list cannot load"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("chats"),
            "expected the list_chats error to propagate, got: {err}"
        );
    }

    #[tokio::test]
    async fn create_chat_fails_when_workdir_persist_fails() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store.clone()).await;
        // Break ONLY the state table AFTER startup: insert_chat (chats
        // table) still succeeds, save_state_entry fails — the exact
        // workdir-persist failure path. (list_chats joins state, so the
        // drop must come after the cache is populated.)
        sqlx::query("DROP TABLE state")
            .execute(&store.pool)
            .await
            .expect("drop state table");

        // An existing path so the failure comes from persistence, not path
        // resolution.
        let dir = tempdir().unwrap();
        let result = state
            .create_chat(
                &sess(&state, "s1").await,
                "broken",
                dir.path().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await;
        assert!(result.err().is_some(), "expected Err, got Ok");
        // Best-effort cleanup removed the dangling chat row (direct chats-
        // table query — list_chats joins the dropped state table).
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chats")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        // No cache entry was inserted on the Err path.
        assert!(state.manager.chats.read().await.is_empty());
    }

    #[tokio::test]
    async fn create_chat_fails_when_chat_insert_fails() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        // Break ONLY the chats table — after ServerState::new loaded the
        // cache (fail-fast on a broken store prevents building a state
        // in the first place). insert_chat fails first, so create_chat
        // must propagate THAT error — with foreign_keys=ON the chat row never
        // exists, so the downstream workdir save would also fail and mask the
        // real failure with a misleading "workdir" error.
        let state = test_state(store.clone()).await;
        sqlx::query("DROP TABLE chats")
            .execute(&store.pool)
            .await
            .expect("drop chats table");

        let dir = tempdir().unwrap();
        let err = state
            .create_chat(
                &sess(&state, "s1").await,
                "broken",
                dir.path().to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .expect_err("create_chat must fail");
        assert!(
            err.to_string().contains("insert"),
            "expected the insert_chat error to propagate, got: {err}"
        );
        // No cache entry was inserted on the Err path.
        assert!(state.manager.chats.read().await.is_empty());
    }

    #[tokio::test]
    async fn create_chat_rejects_unresolvable_workdir() {
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store.clone()).await;
        let result = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                "/nonexistent/flux-workdir",
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await;
        assert!(result.is_err());
        assert!(state.manager.chats.read().await.is_empty());
        assert!(store.list_chats().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn create_chat_rejects_empty_resolved_workdir() {
        // No fallback: an empty workdir must be rejected outright — the
        // sandbox boundary cannot be empty.
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store.clone()).await;
        let result = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                "",
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await;
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_chat_stores_canonical_workdir() {
        use std::os::unix::fs::symlink;
        let store = Arc::new(Store::open_in_memory().await.unwrap());
        let state = test_state(store.clone()).await;
        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = dir.path().join("link");
        symlink(&real, &link).unwrap();

        let info = state
            .create_chat(
                &sess(&state, "s1").await,
                "c",
                link.to_str().unwrap(),
                flux_chat::ResolvedPin {
                    provider: Arc::new(crate::test_util::DummyProvider),
                    id: "default".into(),
                    model: String::new(),
                },
            )
            .await
            .unwrap();
        let persisted = store.load_state(&info.chat_id).await.unwrap();
        assert_eq!(
            persisted.get("workdir").map(String::as_str),
            Some(std::fs::canonicalize(&real).unwrap().to_str().unwrap())
        );
    }
}

// R2 sequence unit tests live with the fanout (router.rs) — the stamping
// became direct proto-field writes there.
