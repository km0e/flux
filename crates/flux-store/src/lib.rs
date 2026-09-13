//! SQLite-backed data store.
//!
//! Unified persistence for chat metadata, messages, runtime state,
//! the provider/model registries, MCP launch rows, and buffered tool
//! outputs.
//!
//! Uses `sqlx::SqlitePool` for native async database access
//! with WAL-mode concurrency.

pub mod buf;
pub mod chats;
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

/// Summary of a chat returned by [`Store::list_chats`]. `workdir`/pin
/// fields come from the `state` table via LEFT JOIN — absent entries are
/// `None` (workdir-less chats render blank).
#[derive(Debug, Serialize)]
pub struct ChatSummary {
    pub chat_id: String,
    pub name: String,
    pub created_at: String,
    /// The chat's most recent message-append time — the sidebar's recency
    /// key (falls back to `created_at` for rows predating the column).
    pub last_activity_at: String,
    /// Fork provenance: the SOURCE conversation when this chat was forked
    /// (`None` = not a fork).
    pub forked_from_chat: Option<String>,
    pub workdir: Option<String>,
    /// The chat's pinned provider registry id. Required at chat creation,
    /// so server-created rows always carry it — `None` only for rows older
    /// than the requirement (no compat) and it errors at spawn, never
    /// falls back.
    pub provider: Option<String>,
    /// The chat's resolved model string. Required at chat creation (no
    /// default exists anywhere), so server-created rows always carry it —
    /// `None` only for rows older than the requirement; hydration leaves
    /// the pin unresolved and the spawn path names the dead pin.
    pub model: Option<String>,
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

        // The one-time auto-vacuum conversion (including the rebuild VACUUM
        // for legacy NONE-mode files) runs HERE — at open, always before the
        // listener binds — so the serving window never sees a whole-db VACUUM.
        Self::ensure_incremental_auto_vacuum(&pool).await?;

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

    /// Return accumulated free pages to the OS — `PRAGMA incremental_vacuum`
    /// (the database runs `auto_vacuum = INCREMENTAL`, ensured at open by
    /// [`Store::ensure_incremental_auto_vacuum`]). A short normal write
    /// transaction: unlike a full VACUUM it never rewrites the whole
    /// database under an exclusive lock, so it is safe while serving.
    pub async fn vacuum_if_needed(&self) -> Result<()> {
        let freelist: i64 = sqlx::query_scalar("PRAGMA freelist_count;")
            .fetch_one(&self.pool)
            .await
            .unwrap_or(0);
        if freelist < 1000 {
            return Ok(());
        }
        tracing::info!(freelist, "running incremental vacuum");
        sqlx::query("PRAGMA incremental_vacuum;")
            .execute(&self.pool)
            .await
            .context("failed to vacuum")?;
        Ok(())
    }

    /// Ensure the database runs `auto_vacuum = INCREMENTAL`.
    ///
    /// The mode is a persistent database property but can only be changed
    /// on an empty database — converting an existing NONE-mode file means
    /// setting the pragma and rebuilding it with one final full VACUUM.
    /// That rebuild happens HERE, at open time (on a fresh file the VACUUM
    /// is a no-op), so serving-time maintenance never needs more than
    /// [`Store::vacuum_if_needed`]'s short incremental transaction.
    async fn ensure_incremental_auto_vacuum(pool: &sqlx::SqlitePool) -> Result<()> {
        // 0 = NONE, 1 = FULL, 2 = INCREMENTAL. flux only ever writes 2;
        // anything else is a fresh file or a pre-conversion legacy state.
        let mode: i64 = sqlx::query_scalar("PRAGMA auto_vacuum;")
            .fetch_one(pool)
            .await
            .unwrap_or(0);
        if mode == 2 {
            return Ok(());
        }
        // Both statements run on ONE pooled connection: the pragma arms the
        // new mode on this connection, the rebuild VACUUM applies it for good.
        let mut conn = pool
            .acquire()
            .await
            .context("failed to acquire connection")?;
        sqlx::query("PRAGMA auto_vacuum = INCREMENTAL;")
            .execute(&mut *conn)
            .await
            .context("failed to set auto_vacuum = INCREMENTAL")?;
        sqlx::query("VACUUM;")
            .execute(&mut *conn)
            .await
            .context("failed to convert database to incremental auto-vacuum")?;
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
        store.insert_chat("chat-old", "Old").await.unwrap();
        store.insert_chat("chat-new", "New").await.unwrap();

        // A chat's first activity is its creation: both DEFAULT stamps land
        // equal (the same statement, the same second).
        let (created, activity): (String, String) =
            sqlx::query_as("SELECT created_at, last_activity_at FROM chats WHERE id = 'chat-new'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert_eq!(created, activity);

        // The stamps carry second precision, so two inserts inside one
        // wall-clock second tie — backdate the two rows to explicit,
        // distinct stamps. The assertions below still exercise the REAL
        // paths (the ORDER BY and the append's atomic touch); only the
        // wall-clock granularity is fixed.
        sqlx::query(
            "UPDATE chats SET created_at = '2020-01-01T00:00:01Z', \
              last_activity_at = '2020-01-01T00:00:01Z' WHERE id = 'chat-old'",
        )
        .execute(&store.pool)
        .await
        .unwrap();
        sqlx::query(
            "UPDATE chats SET created_at = '2020-01-01T00:00:02Z', \
              last_activity_at = '2020-01-01T00:00:02Z' WHERE id = 'chat-new'",
        )
        .execute(&store.pool)
        .await
        .unwrap();

        // Activity order: newest first.
        let list = store.list_chats().await.unwrap();
        assert_eq!(&list[0].chat_id, "chat-new");

        // Appending to the OLD chat touches its activity stamp — it bubbles
        // to the top (the fresh stamp lands past every backdated one) and
        // its summary carries the new time, not creation.
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

    #[tokio::test]
    async fn fresh_db_runs_incremental_auto_vacuum() {
        let store = test_store().await;
        let mode: i64 = sqlx::query_scalar("PRAGMA auto_vacuum;")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(mode, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_converts_legacy_none_mode_db() {
        let path =
            std::env::temp_dir().join(format!("flux-store-legacy-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // A legacy NONE-mode file, created without Store's conversion.
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect(&format!("sqlite:{}?mode=rwc", path.display()))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t(v TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES ('x')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM t").execute(&pool).await.unwrap();
        let before: i64 = sqlx::query_scalar("PRAGMA auto_vacuum;")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(before, 0);
        pool.close().await;
        // Opening through Store converts it — before any serving.
        let store = Store::open(&path).await.unwrap();
        let mode: i64 = sqlx::query_scalar("PRAGMA auto_vacuum;")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(mode, 2);
        drop(store);
        let _ = std::fs::remove_file(&path);
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
