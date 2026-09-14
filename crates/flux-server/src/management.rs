//! Management-plane operations: the Connect surface's grpc shims and the
//! MCP supervisor's event consumer call the SAME functions, so every
//! mutation path — UI action or self-healing respawn — shares one
//! behavior.
//!
//! Each mutation is persist-first (the row IS the truth source); the
//! return value is the reply payload with the inline `error` (D4':
//! request-scoped failures are UI data, never a transport status). The
//! BROADCAST is the caller's job, AFTER the reply goes out — the caller
//! owns the ack-before-broadcast ordering.

use crate::registry::ProviderRegistry;
use flux_session::ServerState;
use std::sync::Arc;

// ── Providers ───────────────────────────────────────────────────────────────

/// The add/remove reply payload (id echoed; validation/persist failures
/// inline).
#[derive(Debug)]
pub(crate) struct ProviderMutation {
    pub id: String,
    pub error: Option<String>,
}

/// Register a provider: persist to the database FIRST (an ack implies
/// durability), then broadcast the fresh registry to every session.
pub(crate) async fn add_provider(
    registry: &ProviderRegistry,
    row: flux_store::providers::ProviderRow,
) -> ProviderMutation {
    let id = row.id.clone();
    match registry.add(row).await {
        Ok(_) => ProviderMutation { id, error: None },
        Err(e) => ProviderMutation {
            id,
            error: Some(e.to_string()),
        },
    }
}

/// Remove a provider. Chats pinned to it keep their pin and fail on the
/// next round with an error naming the dead pin (recovery: switch).
pub(crate) async fn remove_provider(registry: &ProviderRegistry, id: String) -> ProviderMutation {
    match registry.remove(&id).await {
        Ok(()) => ProviderMutation { id, error: None },
        Err(e) => ProviderMutation {
            id,
            error: Some(e.to_string()),
        },
    }
}

// ── Models (LOCAL saved models) ────────────────────────────────────────────

/// The save/remove reply payload.
#[derive(Debug)]
pub(crate) struct ModelMutation {
    pub provider: String,
    pub model: String,
    /// Whether a models.dev enrichment landed (the create path).
    pub enriched: bool,
    pub error: Option<String>,
}

/// The sync reply payload.
#[derive(Debug)]
pub(crate) struct ModelSyncOutcome {
    pub provider: Option<String>,
    pub updated: u32,
    pub error: Option<String>,
}

/// Save (create or edit): resolve the create-path models.dev enrichment
/// BEFORE the write (one store write carries both columns; an edit keeps
/// the stored meta untouched). Enrichment is best-effort: a failed fetch
/// still saves the row. The caller owns the post-reply side effects:
/// broadcast the fresh list, then rebuild exactly the chats pinned to the
/// row (params bake into the request at connection begin).
pub(crate) async fn save_model(
    registry: &ProviderRegistry,
    provider: String,
    model: String,
    mut params: serde_json::Value,
) -> ModelMutation {
    let exists = match registry.model_exists(&provider, &model).await {
        Ok(v) => v,
        Err(e) => {
            return ModelMutation {
                provider,
                model,
                enriched: false,
                error: Some(e.to_string()),
            };
        }
    };
    let mut meta = if exists {
        match registry.model_meta(&provider, &model).await {
            Ok(m) => m,
            Err(e) => {
                return ModelMutation {
                    provider,
                    model,
                    enriched: false,
                    error: Some(e.to_string()),
                };
            }
        }
    } else {
        serde_json::json!({})
    };
    let mut enriched = false;
    if !exists {
        // Fresh row: match models.dev once. The snapshot's context /
        // max-output limits pre-fill the matching params the client left
        // unset — the auto-fill the import flow relies on.
        if let Some(url) = registry.provider_url(&provider) {
            match registry.models_dev().meta_for(&url, &model, false).await {
                Ok(Some(m)) => {
                    fill_params_from_meta(&mut params, &m);
                    meta = m;
                    enriched = true;
                    tracing::info!(provider = %provider, model = %model,
                        "models.dev enrichment landed");
                }
                Ok(None) => {
                    tracing::debug!(provider = %provider, model = %model,
                        "no models.dev match; saving without meta");
                }
                Err(e) => {
                    // The fetch itself already warned at the source
                    // (models_dev::catalog); this names the affected row.
                    tracing::warn!(provider = %provider, model = %model, error = %e,
                        "models.dev enrichment failed; saving without meta");
                }
            }
        }
    }
    match registry.save_model(&provider, &model, &params, &meta).await {
        Ok(()) => ModelMutation {
            provider,
            model,
            enriched,
            error: None,
        },
        Err(e) => ModelMutation {
            provider,
            model,
            enriched: false,
            error: Some(e.to_string()),
        },
    }
}

/// Remove: persist-first. The caller owns the post-reply side effects:
/// broadcast the fresh list, then rebuild the matching chats (their
/// pinned params default back to unset).
pub(crate) async fn remove_model(
    registry: &ProviderRegistry,
    provider: String,
    model: String,
) -> ModelMutation {
    match registry.remove_model(&provider, &model).await {
        Ok(()) => ModelMutation {
            provider,
            model,
            enriched: false,
            error: None,
        },
        Err(e) => ModelMutation {
            provider,
            model,
            enriched: false,
            error: Some(e.to_string()),
        },
    }
}

/// Sync: re-match saved rows against models.dev, metadata ONLY — user
/// params are never touched and NO engine rebuild runs (meta is
/// display-only). An explicit sync force-refreshes the catalog; the fetch
/// failure rides `error` and counts as "nothing updated" for the scope.
pub(crate) async fn sync_models(
    registry: &ProviderRegistry,
    provider: Option<String>,
    model: Option<String>,
) -> ModelSyncOutcome {
    let mut updated: u32 = 0;
    let mut sync_error: Option<String> = None;
    let pairs = match (&provider, &model) {
        (Some(p), Some(m)) => vec![(p.clone(), m.clone())],
        (Some(p), None) => registry
            .saved_model_pairs()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|(rp, _)| rp == p)
            .collect(),
        (None, _) => registry.saved_model_pairs().await.unwrap_or_default(),
    };
    if let Some(url) = provider.as_deref().and_then(|p| registry.provider_url(p)) {
        for (rp, rm) in pairs {
            match registry.models_dev().meta_for(&url, &rm, true).await {
                Ok(Some(meta)) => match registry.update_model_meta(&rp, &rm, &meta).await {
                    Ok(true) => updated += 1,
                    Ok(false) => {}
                    Err(e) => {
                        sync_error = Some(e.to_string());
                        break;
                    }
                },
                Ok(None) => {}
                Err(e) => {
                    sync_error = Some(e);
                    break;
                }
            }
        }
    } else if provider.is_some() {
        sync_error = Some("unknown provider id".to_string());
    }
    ModelSyncOutcome {
        provider,
        updated,
        error: sync_error,
    }
}

// ── MCP servers ────────────────────────────────────────────────────────────

/// The add/remove reply payload. Two failure phases are distinct: the
/// PERSIST error means the row never landed (no broadcast); the APPLY
/// error means the row landed but the live spawn failed (the ack carries
/// it inline and the broadcast STILL lists the row — the next restart
/// retries). The caller folds them into its reply's single `error` field.
#[derive(Debug)]
pub(crate) struct McpMutation {
    pub id: String,
    pub error: Option<String>,
    pub apply_error: Option<String>,
    /// Per-tool registration outcomes (a successful live apply — partial
    /// success included; empty on any failure path).
    pub results: Vec<crate::mcp::ToolRegistration>,
}

/// Add an MCP server: persist FIRST (the row is the launch list — a failed
/// connect is retried at the next process start), then apply LIVE (spawn +
/// register into the global registry), then rebuild every chat's engine so
/// the next round sees the fresh tool set. The apply's failure rides the
/// ack inline while the row stays.
pub(crate) async fn add_mcp_server(
    state: &ServerState,
    mcp: &Arc<crate::mcp::McpManager>,
    row: flux_store::mcp::McpServerRow,
) -> McpMutation {
    let id = row.id.clone();
    match crate::mcp::add(&state.store, row.clone()).await {
        Ok(_) => {
            let (apply_error, results) = match mcp.apply_add(&id, &row).await {
                Ok(outcomes) => {
                    state.restart_all_chats().await;
                    (None, outcomes)
                }
                Err(e) => {
                    tracing::warn!(id = %id, error = %e, "MCP live apply failed; the row stays");
                    (Some(e.to_string()), Vec::new())
                }
            };
            McpMutation {
                id,
                error: None,
                apply_error,
                results,
            }
        }
        Err(e) => McpMutation {
            id,
            error: Some(e.to_string()),
            apply_error: None,
            results: Vec::new(),
        },
    }
}

/// Remove an MCP server: persist-first, then live removal (a failed
/// removal only logs — the row is already gone from the launch list).
pub(crate) async fn remove_mcp_server(
    state: &ServerState,
    mcp: &Arc<crate::mcp::McpManager>,
    id: String,
) -> McpMutation {
    match crate::mcp::remove(&state.store, &id).await {
        Ok(()) => {
            match mcp.apply_remove(&id) {
                Ok(true) => state.restart_all_chats().await,
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(id = %id, error = %e, "MCP live removal failed");
                }
            }
            McpMutation {
                id,
                error: None,
                apply_error: None,
                results: Vec::new(),
            }
        }
        Err(e) => McpMutation {
            id,
            error: Some(e.to_string()),
            apply_error: None,
            results: Vec::new(),
        },
    }
}

// ── Skills ─────────────────────────────────────────────────────────────────

/// The add/remove reply payload (name present on success).
#[derive(Debug)]
pub(crate) struct SkillMutation {
    pub name: Option<String>,
    pub error: Option<String>,
}

/// The skills list: the global skills dir plus — when a chat id is given —
/// that chat's workdir-local skills (read-only; they live in the user's
/// repository).
pub(crate) async fn list_skills(
    state: &ServerState,
    chat_id: Option<String>,
) -> Vec<flux_proto::flux::v1::SkillSummary> {
    let project_dir = match &chat_id {
        Some(id) => {
            let guard = state.chat_info_guard().await;
            guard
                .chats_owned()
                .into_iter()
                .find(|c| c.chat_id == id.as_str())
                .map(|c| {
                    std::path::PathBuf::from(c.workdir)
                        .join(".flux")
                        .join("skills")
                })
        }
        None => None,
    };
    crate::skills::summaries(
        crate::skills::global_skills_dir().as_deref(),
        project_dir.as_deref(),
    )
}

/// Install a skill: exactly one source — a local directory path OR a git
/// URL. Skills are pure filesystem content, so the install is IMMEDIATELY
/// visible to every chat's next `skill_list` call (no restart, nothing
/// persisted beyond the directory itself).
pub(crate) async fn add_skill(
    path: Option<String>,
    url: Option<String>,
    subpath: Option<String>,
) -> SkillMutation {
    let outcome = match (path.as_deref(), url.as_deref()) {
        (Some(_), Some(_)) => Err("give either a local path or a URL — not both".to_string()),
        (Some(p), None) => crate::skills::install_from_path(p).await,
        (None, Some(u)) => crate::skills::install_from_url(u, subpath.as_deref()).await,
        (None, None) => Err("give a local path or a git URL".to_string()),
    };
    match outcome {
        Ok(name) => SkillMutation {
            name: Some(name),
            error: None,
        },
        Err(e) => SkillMutation {
            name: None,
            error: Some(e),
        },
    }
}

/// Remove a skill from the GLOBAL skills dir by name.
pub(crate) async fn remove_skill(name: String) -> SkillMutation {
    match crate::skills::remove_skill(&name).await {
        Ok(()) => SkillMutation {
            name: Some(name),
            error: None,
        },
        Err(e) => SkillMutation {
            name: None,
            error: Some(e),
        },
    }
}

// ── Broadcasts + helpers (module-private) ──────────────────────────────────

/// Broadcast the fresh provider registry to EVERY session after a
/// successful add/remove (the chat-list broadcast pattern: one element,
/// fanned out over the identity registry).
pub(crate) async fn broadcast_providers(state: &ServerState, registry: &ProviderRegistry) {
    use flux_proto::flux::v1::subscribe_response::Kind;
    let el = flux_proto::flux::v1::SubscribeResponse {
        chat_seq: 0,
        chat_id: String::new(),
        kind: Some(Kind::Providers(flux_proto::flux::v1::ProvidersBroadcast {
            providers: registry.summaries(),
        })),
    };
    state.broadcast_element(el).await;
}

/// Push the saved-model list to every session — after a successful
/// model save/remove/sync (the `providers`-broadcast pattern).
pub(crate) async fn broadcast_models(state: &ServerState, registry: &ProviderRegistry) {
    use flux_proto::flux::v1::subscribe_response::Kind;
    let el = flux_proto::flux::v1::SubscribeResponse {
        chat_seq: 0,
        chat_id: String::new(),
        kind: Some(Kind::Models(flux_proto::flux::v1::ModelsBroadcast {
            models: registry.model_summaries(),
        })),
    };
    state.broadcast_element(el).await;
}

/// Broadcast the fresh MCP-server list to every session after a
/// successful add/remove.
pub(crate) async fn broadcast_mcp_servers(state: &ServerState, mcp: &crate::mcp::McpManager) {
    use flux_proto::flux::v1::subscribe_response::Kind;
    match crate::mcp::summaries(&state.store, &mcp.states(), &mcp.tool_names()).await {
        Ok(servers) => {
            let el = flux_proto::flux::v1::SubscribeResponse {
                chat_seq: 0,
                chat_id: String::new(),
                kind: Some(Kind::McpServers(
                    flux_proto::flux::v1::McpServersBroadcast { servers },
                )),
            };
            state.broadcast_element(el).await;
        }
        Err(e) => tracing::warn!(error = %e, "failed to load MCP servers for broadcast"),
    }
}

/// Broadcast one rate-limited MCP server notice (F-10b) to every session.
pub(crate) async fn broadcast_mcp_notice(
    state: &ServerState,
    server_id: &str,
    level: &str,
    message: &str,
) {
    use flux_proto::flux::v1::subscribe_response::Kind;
    let el = flux_proto::flux::v1::SubscribeResponse {
        chat_seq: 0,
        chat_id: String::new(),
        kind: Some(Kind::McpNotice(flux_proto::flux::v1::McpNotice {
            server_id: server_id.to_string(),
            level: level.to_string(),
            message: message.to_string(),
        })),
    };
    state.broadcast_element(el).await;
}

/// Broadcast the fresh global skill list to every session after a
/// successful add/remove.
pub(crate) async fn broadcast_skills(state: &ServerState) {
    use flux_proto::flux::v1::subscribe_response::Kind;
    let skills = crate::skills::summaries(crate::skills::global_skills_dir().as_deref(), None);
    let el = flux_proto::flux::v1::SubscribeResponse {
        chat_seq: 0,
        chat_id: String::new(),
        kind: Some(Kind::Skills(flux_proto::flux::v1::SkillsBroadcast {
            skills,
        })),
    };
    state.broadcast_element(el).await;
}

/// Pre-fill the create-path params from a models.dev snapshot: the
/// snapshot's context / max-output limits become the matching params the
/// client left unset. User-supplied values always win.
fn fill_params_from_meta(params: &mut serde_json::Value, meta: &serde_json::Value) {
    let Some(obj) = params.as_object_mut() else {
        return;
    };
    if let Some(ctx) = meta.get("context_length").and_then(|v| v.as_u64()) {
        obj.entry("context_length")
            .or_insert(serde_json::json!(ctx));
    }
    if let Some(out) = meta.get("max_output").and_then(|v| v.as_u64()) {
        obj.entry("max_tokens").or_insert(serde_json::json!(out));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_params_from_meta_prefills_only_unset_keys() {
        let mut params = serde_json::json!({"temperature": 0.7});
        fill_params_from_meta(
            &mut params,
            &serde_json::json!({"context_length": 128000, "max_output": 8192}),
        );
        // User-supplied values win; unset keys get prefilled.
        assert_eq!(
            params,
            serde_json::json!({"temperature": 0.7, "context_length": 128000, "max_tokens": 8192})
        );
        // The second pass is a no-op (already-set keys are never replaced).
        fill_params_from_meta(&mut params, &serde_json::json!({"context_length": 128000}));
        assert_eq!(params["context_length"], 128000);
    }
}
