//! MCP-server persistence.
//!
//! The ONLY home of the MCP launch list (the server has no config file):
//! one row per external MCP server to connect at startup. Two transports
//! share the table — `kind='stdio'` rows carry the child-process launch
//! (command/args/env), `kind='http'` rows the Streamable HTTP endpoint
//! (url/headers). The UI manages the rows (mcp_add / mcp_remove);
//! changes take effect live (persist-first + apply). `args` / `env` /
//! `headers` are stored as JSON (array / object / object) — env and
//! headers VALUES live here but never leave the server over the wire
//! (secrets parity with the provider api_key).

use super::Store;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

/// How flux connects to one registered MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpServerKind {
    /// A child process flux spawns (stdio transport).
    #[default]
    Stdio,
    /// A remote Streamable HTTP endpoint.
    Http,
}

impl fmt::Display for McpServerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            McpServerKind::Stdio => "stdio",
            McpServerKind::Http => "http",
        })
    }
}

impl FromStr for McpServerKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "stdio" => Ok(McpServerKind::Stdio),
            "http" => Ok(McpServerKind::Http),
            other => anyhow::bail!("unknown MCP server kind: {other}"),
        }
    }
}

/// One MCP-server row: the connection config plus the registry id the UI
/// addresses it by. A stdio row reads `command`/`args`/`env`; an http row
/// reads `url`/`headers` (the other side stays empty — the CHECK-free
/// validation lives at the write point, `flux-server`'s `add`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerRow {
    pub id: String,
    pub kind: McpServerKind,
    /// Executable to run (stdio rows).
    pub command: String,
    /// Arguments passed to the executable (stdio rows).
    pub args: Vec<String>,
    /// Extra environment variables (stdio rows).
    pub env: HashMap<String, String>,
    /// The Streamable HTTP endpoint URL (http rows).
    pub url: String,
    /// Headers sent with every HTTP request (http rows; auth rides here).
    pub headers: HashMap<String, String>,
}

impl McpServerRow {
    /// A stdio row (the common test/default shape).
    pub fn stdio(id: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: McpServerKind::Stdio,
            command: command.into(),
            args: Vec::new(),
            env: HashMap::new(),
            url: String::new(),
            headers: HashMap::new(),
        }
    }

    /// An http row.
    pub fn http(id: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: McpServerKind::Http,
            command: String::new(),
            args: Vec::new(),
            env: HashMap::new(),
            url: url.into(),
            headers: HashMap::new(),
        }
    }
}

impl Store {
    /// All MCP-server rows (unsorted — ordering is a consumer concern).
    /// A row whose JSON columns fail to parse is SKIPPED with a warning
    /// (rows are server-written; this is defensive, never expected).
    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServerRow>> {
        let rows: Vec<(String, String, String, String, String, String, String)> =
            sqlx::query_as("SELECT id, kind, command, args, env, url, headers FROM mcp_servers")
                .fetch_all(&self.pool)
                .await?;
        let mut out = Vec::with_capacity(rows.len());
        for (id, kind, command, args, env, url, headers) in rows {
            let parsed = (|| {
                let kind: McpServerKind = kind.parse()?;
                let args: Vec<String> = serde_json::from_str(&args)?;
                let env: HashMap<String, String> = serde_json::from_str(&env)?;
                let headers: HashMap<String, String> = serde_json::from_str(&headers)?;
                Ok::<_, anyhow::Error>((kind, args, env, headers))
            })();
            match parsed {
                Ok((kind, args, env, headers)) => out.push(McpServerRow {
                    id,
                    kind,
                    command,
                    args,
                    env,
                    url,
                    headers,
                }),
                Err(e) => {
                    tracing::warn!(id = %id, error = %e, "skipping MCP row with corrupt data");
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
        let headers =
            serde_json::to_string(&row.headers).context("failed to encode MCP headers")?;
        match sqlx::query(
            "INSERT INTO mcp_servers (id, kind, command, args, env, url, headers) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(&row.id)
        .bind(row.kind.to_string())
        .bind(&row.command)
        .bind(args)
        .bind(env)
        .bind(&row.url)
        .bind(headers)
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

    fn stdio_row(id: &str) -> McpServerRow {
        McpServerRow {
            args: vec!["-y".to_string(), "@scope/server".to_string()],
            env: HashMap::from([("KEY".to_string(), "value".to_string())]),
            ..McpServerRow::stdio(id, "npx")
        }
    }

    fn http_row(id: &str) -> McpServerRow {
        McpServerRow {
            headers: HashMap::from([("Authorization".to_string(), "Bearer tok".to_string())]),
            ..McpServerRow::http(id, "https://example.com/mcp")
        }
    }

    #[tokio::test]
    async fn mcp_servers_round_trip() {
        let store = test_store().await;
        assert!(store.list_mcp_servers().await.unwrap().is_empty());
        store.insert_mcp_server(&stdio_row("fs")).await.unwrap();
        store.insert_mcp_server(&http_row("remote")).await.unwrap();
        store
            .insert_mcp_server(&McpServerRow::stdio("bare", "srv"))
            .await
            .unwrap();
        let mut rows = store.list_mcp_servers().await.unwrap();
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(rows.len(), 3);
        // The defaults round-trip: an http row keeps empty command/args/env,
        // a stdio row empty url/headers, a bare stdio row everything empty.
        assert_eq!(rows[0], McpServerRow::stdio("bare", "srv"));
        assert_eq!(rows[1], stdio_row("fs"));
        assert_eq!(rows[2], http_row("remote"));
    }

    #[tokio::test]
    async fn duplicate_id_reports_false() {
        let store = test_store().await;
        assert!(store.insert_mcp_server(&stdio_row("fs")).await.unwrap());
        assert!(!store.insert_mcp_server(&stdio_row("fs")).await.unwrap());
        // The unique constraint spans kinds — a different kind is still a
        // duplicate id.
        assert!(!store.insert_mcp_server(&http_row("fs")).await.unwrap());
    }

    #[tokio::test]
    async fn delete_reports_existence() {
        let store = test_store().await;
        store.insert_mcp_server(&stdio_row("fs")).await.unwrap();
        assert!(store.delete_mcp_server("fs").await.unwrap());
        assert!(!store.delete_mcp_server("fs").await.unwrap());
        assert!(store.list_mcp_servers().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn corrupt_json_skips_the_row() {
        // The kind CHECK rejects bad kinds at the SQL layer; a corrupt JSON
        // column is the defensive-parse path (server-written rows — never
        // expected, never fatal).
        let store = test_store().await;
        store.insert_mcp_server(&stdio_row("good")).await.unwrap();
        sqlx::query("INSERT INTO mcp_servers (id, headers) VALUES ('bad', 'not-json')")
            .execute(&store.pool)
            .await
            .unwrap();
        let rows = store.list_mcp_servers().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "good");
    }

    // ── McpServerKind ──

    #[test]
    fn kind_parse_round_trip() {
        assert_eq!(
            "stdio".parse::<McpServerKind>().unwrap(),
            McpServerKind::Stdio
        );
        assert_eq!(
            "http".parse::<McpServerKind>().unwrap(),
            McpServerKind::Http
        );
        assert!("gopher".parse::<McpServerKind>().is_err());
        assert_eq!(McpServerKind::default(), McpServerKind::Stdio);
        assert_eq!(McpServerKind::Http.to_string(), "http");
    }
}
