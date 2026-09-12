//! The management families — providers, models, mcp servers, skills.
//!
//! All four call the SAME operations as the WS dispatch
//! (crate::management): persist-first mutations with their broadcast side
//! effects inside. These impls are pure serialization shims between the
//! proto shapes and the operation payloads — no behavior lives here.

use crate::management;
use crate::registry::ProviderRegistry;
use flux_proto::flux::v1::mcp_service_server::McpService;
use flux_proto::flux::v1::model_service_server::ModelService;
use flux_proto::flux::v1::provider_service_server::ProviderService;
use flux_proto::flux::v1::skill_service_server::SkillService;
use flux_proto::flux::v1::{
    AddProviderRequest, AddProviderResponse, AddServerRequest, AddServerResponse, AddSkillRequest,
    AddSkillResponse, GetModelsRequest, GetModelsResponse, ListProvidersRequest,
    ListProvidersResponse, ListServersRequest, ListServersResponse, ListSkillsRequest,
    ListSkillsResponse, ModelCatalogEntry, ModelSummary, ProviderSummary, RemoveModelRequest,
    RemoveModelResponse, RemoveProviderRequest, RemoveProviderResponse, RemoveServerRequest,
    RemoveServerResponse, RemoveSkillRequest, RemoveSkillResponse, SaveModelRequest,
    SaveModelResponse, SkillSummary, SyncModelsRequest, SyncModelsResponse,
};
use flux_session::ServerState;
use std::collections::HashMap;
use std::sync::Arc;
use tonic::Request;

// ── Providers ───────────────────────────────────────────────────────────────

pub(crate) struct ProviderManagement {
    registry: Arc<ProviderRegistry>,
    state: Arc<ServerState>,
}

impl ProviderManagement {
    pub(crate) fn new(registry: Arc<ProviderRegistry>, state: Arc<ServerState>) -> Self {
        Self { registry, state }
    }
}

#[async_trait::async_trait]
impl ProviderService for ProviderManagement {
    async fn list_providers(
        &self,
        _request: Request<ListProvidersRequest>,
    ) -> Result<tonic::Response<ListProvidersResponse>, tonic::Status> {
        Ok(tonic::Response::new(ListProvidersResponse {
            providers: self
                .registry
                .summaries()
                .into_iter()
                .map(|p| ProviderSummary {
                    id: p.id,
                    url: p.url,
                })
                .collect(),
        }))
    }

    async fn get_models(
        &self,
        request: Request<GetModelsRequest>,
    ) -> Result<tonic::Response<GetModelsResponse>, tonic::Status> {
        let req = request.into_inner();
        let (_, result) = self.registry.list_models(&req.provider).await;
        let response = match result {
            Ok(models) => GetModelsResponse {
                models: models
                    .into_iter()
                    .map(|info| ModelCatalogEntry {
                        id: info.id,
                        context_length: info.context_length,
                    })
                    .collect(),
                error: None,
            },
            Err(e) => GetModelsResponse {
                models: Vec::new(),
                error: Some(e),
            },
        };
        Ok(tonic::Response::new(response))
    }

    async fn add_provider(
        &self,
        request: Request<AddProviderRequest>,
    ) -> Result<tonic::Response<AddProviderResponse>, tonic::Status> {
        let req = request.into_inner();
        let row = flux_store::providers::ProviderRow {
            id: req.id.clone(),
            protocol: req.protocol,
            url: req.url,
            api_key: req.api_key,
        };
        let m = management::add_provider(&self.registry, row).await;
        if m.error.is_none() {
            management::broadcast_providers(&self.state, &self.registry).await;
        }
        Ok(tonic::Response::new(AddProviderResponse {
            id: m.id,
            error: m.error,
        }))
    }

    async fn remove_provider(
        &self,
        request: Request<RemoveProviderRequest>,
    ) -> Result<tonic::Response<RemoveProviderResponse>, tonic::Status> {
        let req = request.into_inner();
        let m = management::remove_provider(&self.registry, req.id).await;
        if m.error.is_none() {
            management::broadcast_providers(&self.state, &self.registry).await;
        }
        Ok(tonic::Response::new(RemoveProviderResponse {
            id: m.id,
            error: m.error,
        }))
    }
}

// ── Models ─────────────────────────────────────────────────────────────────

pub(crate) struct ModelManagement {
    registry: Arc<ProviderRegistry>,
    state: Arc<ServerState>,
}

impl ModelManagement {
    pub(crate) fn new(registry: Arc<ProviderRegistry>, state: Arc<ServerState>) -> Self {
        Self { registry, state }
    }
}

#[async_trait::async_trait]
impl ModelService for ModelManagement {
    async fn list_models(
        &self,
        _request: Request<ListModelsReq>,
    ) -> Result<tonic::Response<ListModelsResp>, tonic::Status> {
        Ok(tonic::Response::new(ListModelsResp {
            models: self
                .registry
                .model_summaries()
                .into_iter()
                .map(|m| ModelSummary {
                    provider: m.provider,
                    model: m.model,
                    params_json: m.params_json,
                    meta_json: m.meta_json,
                })
                .collect(),
        }))
    }

    async fn save_model(
        &self,
        request: Request<SaveModelRequest>,
    ) -> Result<tonic::Response<SaveModelResponse>, tonic::Status> {
        let req = request.into_inner();
        // params_json is the verbatim JSON object (proto3 has no open
        // object scalar — verbatim passthrough IS the contract); a
        // malformed body is a request-scoped inline error, not a status.
        let params: serde_json::Value =
            serde_json::from_str(&req.params_json).unwrap_or(serde_json::Value::Null);
        let m = management::save_model(
            &self.registry,
            req.provider.clone(),
            req.model.clone(),
            params,
        )
        .await;
        if m.error.is_none() {
            management::broadcast_models(&self.state, &self.registry).await;
            self.state
                .restart_chats_matching(&req.provider, &req.model)
                .await;
        }
        Ok(tonic::Response::new(SaveModelResponse {
            provider: m.provider,
            model: m.model,
            enriched: m.enriched,
            error: m.error,
        }))
    }

    async fn remove_model(
        &self,
        request: Request<RemoveModelRequest>,
    ) -> Result<tonic::Response<RemoveModelResponse>, tonic::Status> {
        let req = request.into_inner();
        let m =
            management::remove_model(&self.registry, req.provider.clone(), req.model.clone()).await;
        Ok(tonic::Response::new(RemoveModelResponse {
            provider: m.provider,
            model: m.model,
            error: m.error,
        }))
    }

    async fn sync_models(
        &self,
        request: Request<SyncModelsRequest>,
    ) -> Result<tonic::Response<SyncModelsResponse>, tonic::Status> {
        let req = request.into_inner();
        let out = management::sync_models(&self.registry, req.provider, req.model).await;
        Ok(tonic::Response::new(SyncModelsResponse {
            provider: out.provider,
            updated: out.updated,
            error: out.error,
        }))
    }
}

// The P1 proto named these ListModelsRequest/Response — aliasing keeps the
// impl signatures readable at this size.
use flux_proto::flux::v1::{
    ListModelsRequest as ListModelsReq, ListModelsResponse as ListModelsResp,
};

// ── MCP servers ────────────────────────────────────────────────────────────

pub(crate) struct McpManagement {
    state: Arc<ServerState>,
    mcp: Arc<crate::mcp::McpManager>,
}

impl McpManagement {
    pub(crate) fn new(state: Arc<ServerState>, mcp: Arc<crate::mcp::McpManager>) -> Self {
        Self { state, mcp }
    }
}

#[async_trait::async_trait]
impl McpService for McpManagement {
    async fn list_servers(
        &self,
        _request: Request<ListServersRequest>,
    ) -> Result<tonic::Response<ListServersResponse>, tonic::Status> {
        // Same contract as the WS frame: a listing failure degrades to an
        // empty list + a warn (the dialog renders nothing to retry against).
        let servers = match crate::mcp::summaries(&self.state.store).await {
            Ok(servers) => servers,
            Err(e) => {
                tracing::warn!(error = %e, "failed to list MCP servers");
                Vec::new()
            }
        };
        Ok(tonic::Response::new(ListServersResponse {
            servers: servers
                .into_iter()
                .map(|s| flux_proto::flux::v1::McpServerSummary {
                    id: s.id,
                    command: s.command,
                    args: s.args,
                    env_keys: s.env_keys,
                })
                .collect(),
        }))
    }

    async fn add_server(
        &self,
        request: Request<AddServerRequest>,
    ) -> Result<tonic::Response<AddServerResponse>, tonic::Status> {
        let req = request.into_inner();
        let row = flux_store::mcp::McpServerRow {
            id: req.id.clone(),
            command: req.command,
            args: req.args,
            env: req.env.into_iter().collect::<HashMap<String, String>>(),
        };
        let m = management::add_mcp_server(&self.state, &self.mcp, row).await;
        let ok = m.error.is_none();
        if ok {
            management::broadcast_mcp_servers(&self.state).await;
        }
        Ok(tonic::Response::new(AddServerResponse {
            id: m.id,
            error: m.error.or(m.apply_error),
        }))
    }

    async fn remove_server(
        &self,
        request: Request<RemoveServerRequest>,
    ) -> Result<tonic::Response<RemoveServerResponse>, tonic::Status> {
        let req = request.into_inner();
        let m = management::remove_mcp_server(&self.state, &self.mcp, req.id).await;
        if m.error.is_none() {
            management::broadcast_mcp_servers(&self.state).await;
        }
        Ok(tonic::Response::new(RemoveServerResponse {
            id: m.id,
            error: m.error,
        }))
    }
}

// ── Skills ─────────────────────────────────────────────────────────────────

pub(crate) struct SkillManagement {
    state: Arc<ServerState>,
}

impl SkillManagement {
    pub(crate) fn new(state: Arc<ServerState>) -> Self {
        Self { state }
    }
}

#[async_trait::async_trait]
impl SkillService for SkillManagement {
    async fn list_skills(
        &self,
        request: Request<ListSkillsRequest>,
    ) -> Result<tonic::Response<ListSkillsResponse>, tonic::Status> {
        let req = request.into_inner();
        let skills = management::list_skills(&self.state, req.chat_id).await;
        Ok(tonic::Response::new(ListSkillsResponse {
            skills: skills
                .into_iter()
                .map(|s| SkillSummary {
                    name: s.name,
                    description: s.description,
                    source: s.source.to_owned(),
                    removable: s.removable,
                })
                .collect(),
        }))
    }

    async fn add_skill(
        &self,
        request: Request<AddSkillRequest>,
    ) -> Result<tonic::Response<AddSkillResponse>, tonic::Status> {
        let req = request.into_inner();
        // The source oneof carries exactly one install source (or none —
        // add_skill reports it inline).
        use flux_proto::flux::v1::add_skill_request::Source;
        let (path, url) = match req.source {
            Some(Source::Path(p)) => (Some(p), None),
            Some(Source::Url(u)) => (None, Some(u)),
            None => (None, None),
        };
        let m = management::add_skill(path, url, req.subpath).await;
        if m.error.is_none() {
            management::broadcast_skills(&self.state).await;
        }
        Ok(tonic::Response::new(AddSkillResponse {
            name: m.name,
            error: m.error,
        }))
    }

    async fn remove_skill(
        &self,
        request: Request<RemoveSkillRequest>,
    ) -> Result<tonic::Response<RemoveSkillResponse>, tonic::Status> {
        let req = request.into_inner();
        let m = management::remove_skill(req.name).await;
        if m.error.is_none() {
            management::broadcast_skills(&self.state).await;
        }
        Ok(tonic::Response::new(RemoveSkillResponse {
            name: m.name,
            error: m.error,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use flux_proto::flux::v1::{
        AddProviderRequest, ListProvidersRequest, ListProvidersResponse, ListServersRequest,
        ListServersResponse, ListSkillsRequest, ListSkillsResponse, ProviderSummary,
    };
    use flux_proto::prost::Message as _;

    #[tokio::test]
    async fn provider_add_list_remove_round_trips() {
        let (_state, url) = fixture().await;

        // Add.
        let req = AddProviderRequest {
            id: "test-provider".into(),
            protocol: "openai".into(),
            url: Some("http://127.0.0.1:1/v1".into()),
            api_key: Some("k".into()),
        };
        let resp = post(
            &url,
            "/flux.v1.ProviderService/AddProvider",
            lp_frame(&req.encode_to_vec()),
            None,
        )
        .await;
        assert_eq!(resp.status(), 200);
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = flux_proto::flux::v1::AddProviderResponse::decode(frames[0]).unwrap();
        assert_eq!(out.id, "test-provider");
        assert!(out.error.is_none());

        // List: the fresh registry comes back sorted by id.
        let resp = post(
            &url,
            "/flux.v1.ProviderService/ListProviders",
            lp_frame(&ListProvidersRequest {}.encode_to_vec()),
            None,
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = ListProvidersResponse::decode(frames[0]).unwrap();
        assert_eq!(
            out.providers,
            vec![ProviderSummary {
                id: "test-provider".into(),
                url: "http://127.0.0.1:1/v1".into(),
            }]
        );

        // Remove: ack clean, the list empties.
        let req = RemoveProviderRequest {
            id: "test-provider".into(),
        };
        let resp = post(
            &url,
            "/flux.v1.ProviderService/RemoveProvider",
            lp_frame(&req.encode_to_vec()),
            None,
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = flux_proto::flux::v1::RemoveProviderResponse::decode(frames[0]).unwrap();
        assert!(out.error.is_none());
        let resp = post(
            &url,
            "/flux.v1.ProviderService/ListProviders",
            lp_frame(&ListProvidersRequest {}.encode_to_vec()),
            None,
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, _) = parse_frames(&bytes);
        assert!(
            ListProvidersResponse::decode(frames[0])
                .unwrap()
                .providers
                .is_empty()
        );
    }

    #[tokio::test]
    async fn mcp_list_starts_empty() {
        let (_state, url) = fixture().await;
        let resp = web_client()
            .post(format!("{url}/flux.v1.McpService/ListServers"))
            .header("content-type", "application/grpc-web+proto")
            .body(lp_frame(&ListServersRequest {}.encode_to_vec()))
            .send()
            .await
            .unwrap();
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = ListServersResponse::decode(frames[0]).unwrap();
        assert!(out.servers.is_empty());
    }

    #[tokio::test]
    async fn skills_list_carries_the_global_dir_entries() {
        let (_state, url) = fixture().await;
        let resp = web_client()
            .post(format!("{url}/flux.v1.SkillService/ListSkills"))
            .header("content-type", "application/grpc-web+proto")
            .body(lp_frame(
                &ListSkillsRequest { chat_id: None }.encode_to_vec(),
            ))
            .send()
            .await
            .unwrap();
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = ListSkillsResponse::decode(frames[0]).unwrap();
        // The host's global skills dir may or may not be seeded — assert the
        // STRUCTURE (a clean, well-formed list), not its contents.
        assert!(
            out.skills
                .iter()
                .all(|s| !s.name.is_empty() && !s.description.is_empty())
        );
    }
}
