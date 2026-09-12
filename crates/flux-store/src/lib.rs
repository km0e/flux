//! SQLite-backed data store.
//!
//! Unified persistence for chat metadata, messages, runtime state,
//! buffered tool outputs, and the feature decision log.
//!
//! Uses `sqlx::SqlitePool` for native async database access
//! with WAL-mode concurrency.

pub mod buf;
pub mod chats;
pub mod feature_log;
pub mod mcp;
pub mod messages;
pub mod models;
pub mod providers;
pub mod state;

use anyhow::{Context, Result};
use serde::Serialize;
use std::path::Path;

/// Central data store backed by SQLite via sqlx.
#[derive(Debug)]
pub struct Store {
    pub pool: sqlx::SqlitePool,
}

/// Summary of a chat returned by [`Store::list_chats`]. `kind`/`workdir`/
/// pin fields come from the `state` table via LEFT JOIN — absent entries
/// are `None` (pre-kind chats default to classic; workdir-less chats
/// render blank).
#[derive(Debug, Serialize)]
pub struct ChatSummary {
    pub chat_id: String,
    pub name: String,
    pub created_at: String,
    /// The chat's most recent message-append time — the sidebar's recency
    /// key (falls back to `created_at` for rows predating the column).
    pub last_activity_at: String,
    pub kind: Option<String>,
    pub workdir: Option<String>,
    /// The chat's pinned provider registry id. Required at `chat_create`,
    /// so server-created rows always carry it — `None` only for rows older
    /// than the requirement (no compat) and it errors at spawn, never
    /// falls back.
    pub provider: Option<String>,
    /// The chat's resolved model string. Required at `chat_create` (no
    /// default exists anywhere), so server-created rows always carry it —
    /// `None` only for rows older than the requirement; hydration leaves
    /// the pin unresolved and the spawn path names the dead pin.
    pub model: Option<String>,
}

/// One entry of a chat's feature decision log.
#[derive(Debug, Serialize)]
pub struct FeatureLogEntry {
    pub id: i64,
    pub summary: String,
    pub created_at: String,
}

/// SQLite PRAGMAs applied on every new connection to the pool.
const CONNECTION_PRAGMAS: &str = "
    PRAGMA journal_mode = WAL;
    PRAGMA synchronous = NORMAL;
    PRAGMA foreign_keys = ON;
    PRAGMA busy_timeout = 5000;
    PRAGMA cache_size = -65536;
    PRAGMA mmap_size = 268435456;
    PRAGMA temp_store = MEMORY;
    PRAGMA wal_autocheckpoint = 2000;
";

impl Store {
    /// Open (or create) the database at `path`, applying any pending migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        Self::open_with(
            &format!("sqlite:{}?mode=rwc", path.display()),
            CONNECTION_PRAGMAS,
            4,
        )
        .await
    }

    /// Create an in-memory Store with all migrations applied.
    pub async fn open_in_memory() -> Result<Store> {
        Self::open_with(
            "sqlite::memory:",
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
            1,
        )
        .await
    }

    /// Open a pool at `url`, applying `pragmas` to every connection and
    /// running pending migrations.
    async fn open_with(url: &str, pragmas: &'static str, max_connections: u32) -> Result<Self> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(max_connections)
            .after_connect(move |conn, _meta| {
                Box::pin(async move { sqlx::query(pragmas).execute(conn).await.map(|_| ()) })
            })
            .connect(url)
            .await
            .context("failed to open database")?;

        let migrator = sqlx::migrate::Migrator::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"),
        )
        .await
        .context("failed to load migrations")?;
        migrator
            .run(&pool)
            .await
            .context("failed to apply migrations")?;

        Ok(Self { pool })
    }

    // ── Maintenance ──

    /// Run VACUUM only when the database has accumulated significant free
    /// pages or WAL backlog.
    pub async fn vacuum_if_needed(&self) -> Result<()> {
        let freelist: i64 = sqlx::query_scalar("PRAGMA freelist_count;")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        if freelist < 1000 {
            return Ok(());
        }
        tracing::info!(freelist, "running VACUUM");
        sqlx::query("VACUUM;")
            .execute(&self.pool)
            .await
            .context("failed to vacuum")?;
        Ok(())
    }

    /// Run PRAGMA optimize after startup to refresh query planner statistics.
    pub async fn optimize(&self) {
        let _ = sqlx::query("PRAGMA optimize;").execute(&self.pool).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::{Message, Role, ToolCall};

    async fn test_store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn chat_list_orders_by_last_activity_and_append_touches_it() {
        let store = test_store().await;
        // Created oldest-first: creation order alone would put "older" on top
        // only by creation DESC... both start at their creation stamp.
        store.insert_chat("chat-old", "Old").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        store.insert_chat("chat-new", "New").await.unwrap();
        // Creation order: newest first.
        let list = store.list_chats().await.unwrap();
        assert_eq!(&list[0].chat_id, "chat-new");
        assert_eq!(list[0].last_activity_at, list[0].created_at);

        // Appending to the OLD chat touches its activity stamp — it bubbles
        // to the top and its summary carries the new time, not creation.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        store
            .append_messages("chat-old", &[Message::user("revive")])
            .await
            .unwrap();
        let list = store.list_chats().await.unwrap();
        assert_eq!(&list[0].chat_id, "chat-old");
        assert!(
            list[0].last_activity_at > list[0].created_at,
            "activity stamp must move past creation on append"
        );
    }

    #[tokio::test]
    async fn messages_round_trip() {
        let store = test_store().await;
        let chat_id = "test-chat";
        store.insert_chat(chat_id, "Test").await.unwrap();
        let messages = vec![
            Message::user("hello"),
            Message {
                role: Role::Assistant,
                content: "hi there".into(),
                reasoning_content: Some("thinking...".into()),
                tool_calls: vec![],
                tool_call_id: None,
            },
        ];
        store.append_messages(chat_id, &messages).await.unwrap();
        let loaded = store.load_messages(chat_id).await.unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, Role::User);
        assert_eq!(loaded[0].content, "hello");
        assert_eq!(loaded[1].role, Role::Assistant);
        assert_eq!(loaded[1].content, "hi there");
        assert_eq!(loaded[1].reasoning_content.as_deref(), Some("thinking..."));
    }

    #[tokio::test]
    async fn reasoning_content_round_trip() {
        let store = test_store().await;
        let chat_id = "rc-chat";
        store.insert_chat(chat_id, "RC Test").await.unwrap();
        let msg = Message {
            role: Role::Assistant,
            content: "answer".into(),
            reasoning_content: Some("deep reasoning".into()),
            tool_calls: vec![],
            tool_call_id: None,
        };
        store.append_messages(chat_id, &[msg]).await.unwrap();
        let loaded = store.load_messages(chat_id).await.unwrap();
        assert_eq!(
            loaded[0].reasoning_content.as_deref(),
            Some("deep reasoning")
        );
    }

    #[tokio::test]
    async fn reasoning_content_null_when_none() {
        let store = test_store().await;
        let chat_id = "null-rc";
        store.insert_chat(chat_id, "Null RC").await.unwrap();
        let msg = Message {
            role: Role::Assistant,
            content: "no thinking".into(),
            reasoning_content: None,
            tool_calls: vec![],
            tool_call_id: None,
        };
        store.append_messages(chat_id, &[msg]).await.unwrap();
        let loaded = store.load_messages(chat_id).await.unwrap();
        assert_eq!(loaded[0].reasoning_content, None);
    }

    #[tokio::test]
    async fn messages_with_tool_calls_round_trip() {
        let store = test_store().await;
        let chat_id = "tc-chat";
        store.insert_chat(chat_id, "TC Test").await.unwrap();
        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"/tmp/test"}"#.into(),
        };
        let messages = vec![
            Message::user("read /tmp/test"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                reasoning_content: Some("need to read file".into()),
                tool_calls: vec![tool_call.clone()],
                tool_call_id: None,
            },
            Message::tool("call_1", "file contents here"),
        ];
        store.append_messages(chat_id, &messages).await.unwrap();
        let loaded = store.load_messages(chat_id).await.unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[1].tool_calls.len(), 1);
        assert_eq!(loaded[1].tool_calls[0].id, "call_1");
        assert_eq!(loaded[1].tool_calls[0].name, "read_file");
        assert_eq!(
            loaded[1].reasoning_content.as_deref(),
            Some("need to read file")
        );
        assert_eq!(loaded[2].role, Role::Tool);
        assert_eq!(loaded[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(loaded[2].content, "file contents here");
    }

    #[tokio::test]
    async fn append_messages_auto_creates_chat() {
        let store = test_store().await;
        store
            .append_messages("auto-chat", &[Message::user("hi")])
            .await
            .unwrap();
        let chats = store.list_chats().await.unwrap();
        assert!(chats.iter().any(|c| c.chat_id == "auto-chat"));
    }

    #[tokio::test]
    async fn state_empty_on_fresh_db() {
        let store = test_store().await;
        let state = store.load_state("no-such-chat").await.unwrap();
        assert!(state.is_empty());
    }

    #[tokio::test]
    async fn vacuum_skips_small_db() {
        let store = test_store().await;
        let result = store.vacuum_if_needed().await;
        assert!(result.is_ok());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_file_db_and_query() {
        let path = std::env::temp_dir().join(format!("flux-store-sqlx-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).await.unwrap();
        store.insert_chat("shared-chat", "Shared").await.unwrap();
        let name: (String,) = sqlx::query_as("SELECT name FROM chats WHERE id = ?1")
            .bind("shared-chat")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(name.0, "Shared");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }

    #[tokio::test]
    async fn approvals_table_never_exists() {
        // The consolidated schema has no approvals table — flux keeps no
        // remembered approval state. This pins the final shape against
        // reintroduction.
        let store = test_store().await;
        let exists: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='approvals'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(exists.0, 0, "approvals table must not exist");
    }

    #[tokio::test]
    async fn delete_chat_removes_state_rows() {
        let store = test_store().await;
        store.insert_chat("dc-chat", "DC").await.unwrap();
        store
            .save_state_entry("dc-chat", "workdir", "/tmp")
            .await
            .unwrap();
        store.delete_chat("dc-chat").await.unwrap();
        let state = store.load_state("dc-chat").await.unwrap();
        assert!(
            state.is_empty(),
            "state rows must cascade-delete, got: {state:?}"
        );
    }
}
