//! MCP-server persistence.
//!
//! The ONLY home of the MCP launch list (the server has no config file):
//! one row per external MCP server to spawn as a child process at startup.
//! The UI manages the rows (mcp_add / mcp_remove); changes take effect at
//! the next restart. `args` / `env` are stored as JSON (array / object) —
//! env VALUES live here but never leave the server over the wire
//! (secrets parity with the provider api_key).

use super::Store;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// One MCP-server row: the launch triple (command, args, env) plus the
/// registry id the UI addresses it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerRow {
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

impl Store {
    /// All MCP-server rows (unsorted — ordering is a consumer concern).
    /// A row whose JSON columns fail to parse is SKIPPED with a warning
    /// (rows are server-written; this is defensive, never expected).
    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerRow>> {
        let rows: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT id, command, args, env FROM mcp_servers")
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, command, args, env) in rows {
            let parsed = (|| {
                let args: Vec<String> = serde_json::from_str(&args)?;
                let env: HashMap<String, String> = serde_json::from_str(&env)?;
                Ok::<_, serde_json::Error>((args, env))
            })();
            match parsed {
                Ok((args, env)) => out.push(McpServerRow {
                    id,
                    command,
                    args,
                    env,
                }),
                Err(e) => {
                    tracing::warn!(id = %id, error = %e, "skipping MCP row with corrupt JSON");
                }
            }
        }
        Ok(out)
    }

    /// Insert one MCP-server row. Returns `Ok(false)` on a duplicate `id`
    /// (the UNIQUE constraint is the race guard for concurrent adds);
    /// other database/JSON errors propagate.
    pub async fn insert_mcp_server(&self, row: &McpServerRow) -> Result<bool> {
        let args = serde_json::to_string(&row.args).context("failed to encode MCP args")?;
        let env = serde_json::to_string(&row.env).context("failed to encode MCP env")?;
        match sqlx::query(
            "INSERT INTO mcp_servers (id, command, args, env) VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&row.id)
        .bind(&row.command)
        .bind(args)
        .bind(env)
        .execute(&self.pool)
        .await
        {
            Ok(_) => Ok(true),
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete one MCP-server row. Returns whether the row existed.
    pub async fn delete_mcp_server(&self, id: &str) -> Result<bool> {
        let result = sqlx::query("DELETE FROM mcp_servers WHERE id = ?1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_store() -> Store {
        Store::open_in_memory().await.unwrap()
    }

    fn row(id: &str) -> McpServerRow {
        McpServerRow {
            id: id.to_string(),
            command: "npx".to_string(),
            args: vec!["-y".to_string(), "@scope/server".to_string()],
            env: HashMap::from([("KEY".to_string(), "value".to_string())]),
        }
    }

    #[tokio::test]
    async fn mcp_servers_round_trip() {
        let store = test_store().await;
        assert!(store.list_mcp_servers().await.unwrap().is_empty());
        store.insert_mcp_server(&row("fs")).await.unwrap();
        store
            .insert_mcp_server(&McpServerRow {
                id: "bare".to_string(),
                command: "srv".to_string(),
                args: vec![],
                env: HashMap::new(),
            })
            .await
            .unwrap();
        let mut rows = store.list_mcp_servers().await.unwrap();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(rows.len(), 2);
        // The defaults round-trip: empty args/env stay empty.
        assert_eq!(
            rows[0],
            McpServerRow {
                id: "bare".to_string(),
                command: "srv".to_string(),
                args: vec![],
                env: HashMap::new(),
            }
        );
        assert_eq!(rows[1], row("fs"));
    }

    #[tokio::test]
    async fn duplicate_id_reports_false() {
        let store = test_store().await;
        assert!(store.insert_mcp_server(&row("fs")).await.unwrap());
        assert!(!store.insert_mcp_server(&row("fs")).await.unwrap());
    }

    #[tokio::test]
    async fn delete_reports_existence() {
        let store = test_store().await;
        store.insert_mcp_server(&row("fs")).await.unwrap();
        assert!(store.delete_mcp_server("fs").await.unwrap());
        assert!(!store.delete_mcp_server("fs").await.unwrap());
        assert!(store.list_mcp_servers().await.unwrap().is_empty());
    }
}
