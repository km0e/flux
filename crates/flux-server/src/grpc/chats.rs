//! The chat family — the conversation control plane over Connect.
//!
//! Identity: every RPC here is chat-scoped and lease-sensitive, so the
//! caller's session token rides the request metadata (`x-flux-session`);
//! unknown/expired tokens are refused at the transport boundary
//! (unauthenticated). The resolved identity handle is the SAME object the
//! Subscribe stream's fanout holds — the lease/viewer machinery never
//! re-derives it from the token string.
//!
//! Behavior lives in flux-session's ops (shared by every transport
//! surface); these impls resolve the caller and map outcomes onto proto
//! shapes.

use crate::registry::ProviderRegistry;
use flux_proto::flux::v1::chat_service_server::ChatService;
use flux_proto::flux::v1::{
    AnswerQuestionRequest, AnswerQuestionResponse, CancelRoundRequest, CancelRoundResponse,
    ChatInfo, ClaimChatRequest, ClaimChatResponse, CloseChatRequest, CloseChatResponse,
    CreateChatRequest, CreateChatResponse, DeleteChatRequest, DeleteChatResponse, ForkChatRequest,
    ForkChatResponse, ListChatsRequest, ListChatsResponse, OpenChatRequest, OpenChatResponse,
    RenameChatRequest, RenameChatResponse, SendMessageRequest, SendMessageResponse,
    SwitchProviderRequest, SwitchProviderResponse,
};
use flux_session::ServerState;
use flux_session::SessionRef;
use flux_session::ops::{
    CancelOutcome, ClaimOutcome, ForkFailure, MutateOutcome, OpenOutcome, QuestionOutcome,
    SendOutcome, SwitchOutcome,
};
use std::sync::Arc;
use tonic::{Request, Status};

pub(crate) struct ChatManagement {
    state: Arc<ServerState>,
    registry: Arc<ProviderRegistry>,
}

impl ChatManagement {
    pub(crate) fn new(state: Arc<ServerState>, registry: Arc<ProviderRegistry>) -> Self {
        Self { state, registry }
    }

    /// Resolve the caller's identity from the request metadata. Every RPC
    /// in this service is chat-scoped, so an absent/unknown token is a
    /// transport-level refusal (the identity IS the transport credential).
    // tonic::Status is large but is THE error type of this surface; boxing
    // would poison every handler signature for no runtime benefit.
    #[allow(clippy::result_large_err)]
    async fn caller<T>(&self, request: &Request<T>) -> Result<SessionRef, Status> {
        let token = request
            .metadata()
            .get("x-flux-session")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Status::unauthenticated("missing x-flux-session metadata"))?;
        self.state
            .session_leases(token)
            .await
            .map(|(s, _)| s)
            .ok_or_else(|| Status::unauthenticated("unknown or expired session"))
    }
}

fn chat_info(info: &flux_session::manager::ChatInfoOwned) -> ChatInfo {
    ChatInfo {
        chat_id: info.chat_id.clone(),
        name: info.name.clone(),
        created_at: info.created_at.clone(),
        last_activity_at: info.last_activity_at.clone(),
        active: info.active,
        workdir: info.workdir.clone(),
        provider: info.provider.clone(),
        model: info.model.clone(),
        forked_from_chat_id: info.forked_from_chat.clone(),
    }
}

/// The lease-gate refusals map onto standard statuses (the
/// transport-level counterparts of the stream's `error{chat_busy}` /
/// `error{chat_not_found}` elements).
fn mutate_status(outcome: MutateOutcome) -> Option<Status> {
    match outcome {
        MutateOutcome::Ok => None,
        MutateOutcome::Busy => Some(Status::failed_precondition(
            "another session holds the chat's lease",
        )),
        MutateOutcome::NotFound => Some(Status::not_found("unknown chat")),
    }
}

#[async_trait::async_trait]
impl ChatService for ChatManagement {
    async fn create_chat(
        &self,
        request: Request<CreateChatRequest>,
    ) -> Result<tonic::Response<CreateChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        let pin = match self.registry.instance(&req.provider, &req.model) {
            Ok((provider, id, model)) => flux_chat::ResolvedPin {
                provider,
                id,
                model,
            },
            Err(e) => {
                return Ok(tonic::Response::new(CreateChatResponse {
                    chat: None,
                    error: Some(format!("provider pin rejected: {e}")),
                }));
            }
        };
        match self
            .state
            .create_chat(&session, &req.name, &req.workdir, pin)
            .await
        {
            Ok(info) => Ok(tonic::Response::new(CreateChatResponse {
                chat: Some(chat_info(&info)),
                error: None,
            })),
            Err(e) => Ok(tonic::Response::new(CreateChatResponse {
                chat: None,
                error: Some(e.to_string()),
            })),
        }
    }

    async fn list_chats(
        &self,
        _request: Request<ListChatsRequest>,
    ) -> Result<tonic::Response<ListChatsResponse>, Status> {
        let owned = {
            let guard = self.state.chat_info_guard().await;
            guard.chats_owned()
        };
        Ok(tonic::Response::new(ListChatsResponse {
            chats: owned.iter().map(chat_info).collect(),
        }))
    }

    async fn open_chat(
        &self,
        request: Request<OpenChatRequest>,
    ) -> Result<tonic::Response<OpenChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        match self.state.open_chat(&session, &req.chat_id).await {
            OpenOutcome::Subscribed => Ok(tonic::Response::new(OpenChatResponse {})),
            OpenOutcome::NotFound => Err(Status::not_found("unknown chat")),
        }
    }

    async fn claim_chat(
        &self,
        request: Request<ClaimChatRequest>,
    ) -> Result<tonic::Response<ClaimChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        // A claim always grants — another holder's lease is STOLEN (the
        // previous holder is demoted in-band via error{chat_busy}), so
        // there is no busy outcome on this path.
        match self.state.claim_chat(&session, &req.chat_id).await {
            ClaimOutcome::Granted => Ok(tonic::Response::new(ClaimChatResponse {
                already_owned: false,
            })),
            ClaimOutcome::AlreadyOwned => Ok(tonic::Response::new(ClaimChatResponse {
                already_owned: true,
            })),
            ClaimOutcome::NotFound => Err(Status::not_found("unknown chat")),
        }
    }

    async fn close_chat(
        &self,
        request: Request<CloseChatRequest>,
    ) -> Result<tonic::Response<CloseChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        self.state.unsubscribe_chat(&session, &req.chat_id).await;
        Ok(tonic::Response::new(CloseChatResponse {}))
    }

    async fn delete_chat(
        &self,
        request: Request<DeleteChatRequest>,
    ) -> Result<tonic::Response<DeleteChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        match self.state.delete_chat(&session, &req.chat_id).await {
            MutateOutcome::Ok => Ok(tonic::Response::new(DeleteChatResponse {})),
            other => Err(mutate_status(other).expect("Ok is the only non-error MutateOutcome")),
        }
    }

    async fn rename_chat(
        &self,
        request: Request<RenameChatRequest>,
    ) -> Result<tonic::Response<RenameChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        match self
            .state
            .rename_chat(&session, &req.chat_id, &req.name)
            .await
        {
            MutateOutcome::Ok => Ok(tonic::Response::new(RenameChatResponse {})),
            other => Err(mutate_status(other).expect("Ok is the only non-error MutateOutcome")),
        }
    }

    async fn send_message(
        &self,
        request: Request<SendMessageRequest>,
    ) -> Result<tonic::Response<SendMessageResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        // The interrupt flag fuses "cancel the live round" + "submit the
        // message" into one operation server-side (R1). `client_msg_id` is
        // the idempotency key: a resend with an accepted key is absorbed as
        // a duplicate instead of enqueueing a second turn.
        match self
            .state
            .send_message_idempotent(
                &session,
                &req.chat_id,
                req.message,
                req.interrupt,
                req.client_msg_id,
            )
            .await
        {
            Ok(SendOutcome::Ok) => Ok(tonic::Response::new(SendMessageResponse {
                duplicate: false,
            })),
            Ok(SendOutcome::Duplicate) => Ok(tonic::Response::new(SendMessageResponse {
                duplicate: true,
            })),
            Ok(SendOutcome::Busy) => Err(Status::failed_precondition(
                "another session holds the chat's lease",
            )),
            Ok(SendOutcome::NotFound) => Err(Status::not_found("unknown chat")),
            Err(e) => Err(Status::internal(format!("send failed: {e}"))),
        }
    }

    async fn cancel_round(
        &self,
        request: Request<CancelRoundRequest>,
    ) -> Result<tonic::Response<CancelRoundResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        match self.state.cancel_chat(&session, &req.chat_id).await {
            CancelOutcome::Ok => Ok(tonic::Response::new(CancelRoundResponse {})),
            CancelOutcome::Busy => Err(Status::failed_precondition(
                "another session holds the chat's lease",
            )),
            CancelOutcome::NotFound => Err(Status::not_found("unknown chat")),
        }
    }

    async fn fork_chat(
        &self,
        request: Request<ForkChatRequest>,
    ) -> Result<tonic::Response<ForkChatResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        // Resolve the source's pin HERE (the registry owns id → instance
        // selection): a dead pin refuses the fork inline, mirroring
        // create_chat's pin gate — forking into a chat that cannot spawn
        // is never the honest outcome.
        let Some((provider_id, model)) = self.state.chat_pin(&req.chat_id).await else {
            return Ok(tonic::Response::new(ForkChatResponse {
                chat: None,
                error: Some("unknown chat".into()),
            }));
        };
        let pin = match self.registry.instance(&provider_id, &model) {
            Ok((provider, id, model)) => flux_chat::ResolvedPin {
                provider,
                id,
                model,
            },
            Err(e) => {
                return Ok(tonic::Response::new(ForkChatResponse {
                    chat: None,
                    error: Some(format!(
                        "the source chat's provider pin rejected: {e} — switch the source chat's provider and retry"
                    )),
                }));
            }
        };
        match self
            .state
            .fork_chat(&session, &req.chat_id, req.fork_point, pin)
            .await
        {
            Ok(info) => Ok(tonic::Response::new(ForkChatResponse {
                chat: Some(chat_info(&info)),
                error: None,
            })),
            Err(ForkFailure::NotFound) => Ok(tonic::Response::new(ForkChatResponse {
                chat: None,
                error: Some("unknown chat".into()),
            })),
            Err(ForkFailure::BadPoint) => Ok(tonic::Response::new(ForkChatResponse {
                chat: None,
                error: Some(
                    "the fork point is not a user message of that conversation — fork from one of your own messages"
                        .into(),
                ),
            })),
            Err(ForkFailure::Internal(e)) => Ok(tonic::Response::new(ForkChatResponse {
                chat: None,
                error: Some(e),
            })),
        }
    }

    async fn switch_provider(
        &self,
        request: Request<SwitchProviderRequest>,
    ) -> Result<tonic::Response<SwitchProviderResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        // Resolve HERE: the registry owns id → instance selection. A
        // resolution failure is request-scoped validation — it rides the
        // inline `error` (D4'), never a transport status; only the lease
        // gate rides statuses.
        let pin = match self.registry.instance(&req.provider, &req.model) {
            Ok((provider, id, model)) => flux_chat::ResolvedPin {
                provider,
                id,
                model,
            },
            Err(e) => {
                return Ok(tonic::Response::new(SwitchProviderResponse {
                    error: Some(format!("provider switch rejected: {e}")),
                }));
            }
        };
        match self
            .state
            .switch_provider(&session, &req.chat_id, pin)
            .await
        {
            Ok(SwitchOutcome::Ok) => {
                Ok(tonic::Response::new(SwitchProviderResponse { error: None }))
            }
            Ok(SwitchOutcome::Busy) => Err(Status::failed_precondition(
                "another session holds the chat's lease",
            )),
            Ok(SwitchOutcome::NotFound) => Err(Status::not_found("unknown chat")),
            Err(e) => Ok(tonic::Response::new(SwitchProviderResponse {
                error: Some(format!("provider switch rejected: {e}")),
            })),
        }
    }

    async fn answer_question(
        &self,
        request: Request<AnswerQuestionRequest>,
    ) -> Result<tonic::Response<AnswerQuestionResponse>, Status> {
        let session = self.caller(&request).await?;
        let req = request.into_inner();
        match self
            .state
            .question_response(&session, &req.chat_id, &req.id, req.answer)
            .await
        {
            // Delivered, and stale answers drop silently — an unknown
            // question is not an error the caller can act on.
            QuestionOutcome::Ok | QuestionOutcome::UnknownQuestion => {
                Ok(tonic::Response::new(AnswerQuestionResponse {}))
            }
            QuestionOutcome::NotOwner => Err(Status::failed_precondition(
                "another session holds the chat's lease",
            )),
            QuestionOutcome::NotFound => Err(Status::not_found("unknown chat")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;

    /// The gRPC status of a trailers-only response (refused calls carry it
    /// in the HTTP headers; successful unary calls carry it in the body's
    /// trailer frame).
    fn grpc_status_headers(resp: &reqwest::Response) -> Option<i32> {
        resp.headers()
            .get("grpc-status")?
            .to_str()
            .ok()
            .and_then(|v| v.parse().ok())
    }

    use super::*;
    use flux_proto::flux::v1::{
        ClaimChatRequest, ClaimChatResponse, CreateChatRequest, CreateChatResponse,
        ListChatsRequest, ListChatsResponse, SendMessageRequest,
    };
    use flux_proto::prost::Message as _;

    /// The lease-gated surface refuses a caller without session metadata
    /// (ListChats is a global list and is deliberately NOT gated).
    #[tokio::test]
    async fn missing_session_metadata_is_unauthenticated() {
        let (_state, url) = fixture().await;
        let resp = web_client()
            .post(format!("{url}/flux.v1.ChatService/SendMessage"))
            .header("content-type", "application/grpc-web+proto")
            .body(lp_frame(
                &SendMessageRequest {
                    chat_id: "c".into(),
                    message: "hi".into(),
                    interrupt: false,
                    client_msg_id: None,
                }
                .encode_to_vec(),
            ))
            .send()
            .await
            .unwrap();
        // An immediately-refused call is a trailers-only response: the
        // grpc-status rides the HTTP HEADERS, the body is empty.
        assert_eq!(grpc_status_headers(&resp), Some(16));
        assert!(
            resp.bytes().await.unwrap().is_empty(),
            "no payload on a refused call"
        );
    }

    /// Attach a real identity (token), then drive create → list → claim →
    /// send over the Connect surface with the token in the metadata. The
    /// claim's history snapshot rides the identity's SINK (single delivery
    /// point) — not the response.
    #[tokio::test]
    async fn create_claim_send_round_trip_with_session_metadata() {
        let (state, url) = fixture().await;

        // A live identity: the browser analog is the Subscribe stream
        // (its token rides the Connect calls as metadata).
        struct NullSink;
        impl flux_session::SessionSink for NullSink {
            fn send(&self, _: flux_proto::flux::v1::SubscribeResponse) -> bool {
                true
            }
            fn send_control(&self, _: flux_proto::flux::v1::SubscribeResponse) -> bool {
                true
            }
        }
        let session = state.attach_session(Arc::new(NullSink)).await;
        let token = session.token().to_owned();

        // Create (pin validation included: an unknown provider fails inline).
        let bad = CreateChatRequest {
            name: "x".into(),
            workdir: "/tmp".into(),
            provider: "nope".into(),
            model: "m".into(),
        };
        let resp = post(
            &url,
            "/flux.v1.ChatService/CreateChat",
            lp_frame(&bad.encode_to_vec()),
            Some(&token),
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(
            trailer_grpc_status(trailer),
            Some(0),
            "inline, not transport"
        );
        let out = CreateChatResponse::decode(frames[0]).unwrap();
        assert!(
            out.error.is_some(),
            "unknown provider rides the inline error"
        );
        assert!(out.chat.is_none());

        // Good create (no provider registered yet → still rejected; register
        // one through the same surface first).
        let add = flux_proto::flux::v1::AddProviderRequest {
            id: "p1".into(),
            protocol: "openai".into(),
            url: Some("http://127.0.0.1:1/v1".into()),
            api_key: Some("k".into()),
        };
        let resp = post(
            &url,
            "/flux.v1.ProviderService/AddProvider",
            lp_frame(&add.encode_to_vec()),
            Some(&token),
        )
        .await;
        assert_eq!(resp.status(), 200);

        let good = CreateChatRequest {
            name: "chat".into(),
            workdir: "/tmp".into(),
            provider: "p1".into(),
            model: "m".into(),
        };
        let resp = post(
            &url,
            "/flux.v1.ChatService/CreateChat",
            lp_frame(&good.encode_to_vec()),
            Some(&token),
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = CreateChatResponse::decode(frames[0]).unwrap();
        assert!(out.error.is_none(), "create failed: {:?}", out.error);
        let chat = out.chat.expect("chat info");
        let chat_id = chat.chat_id.clone();

        // List carries it.
        let resp = post(
            &url,
            "/flux.v1.ChatService/ListChats",
            lp_frame(&ListChatsRequest {}.encode_to_vec()),
            Some(&token),
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, _) = parse_frames(&bytes);
        let out = ListChatsResponse::decode(frames[0]).unwrap();
        assert!(out.chats.iter().any(|c| c.chat_id == chat_id));

        // Claim: granted (the creator's lease was granted at create; this is
        // the already-owned re-claim — snapshot re-delivery rides the sink).
        let resp = post(
            &url,
            "/flux.v1.ChatService/ClaimChat",
            lp_frame(
                &ClaimChatRequest {
                    chat_id: chat_id.clone(),
                }
                .encode_to_vec(),
            ),
            Some(&token),
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = ClaimChatResponse::decode(frames[0]).unwrap();
        assert!(out.already_owned, "creator re-claim is already-owned");

        // Send: accepted (the machine queues the turn; the scripted round
        // machinery is not wired here — no events asserted).
        let resp = post(
            &url,
            "/flux.v1.ChatService/SendMessage",
            lp_frame(
                &SendMessageRequest {
                    chat_id: chat_id.clone(),
                    message: "hi".into(),
                    interrupt: false,
                    client_msg_id: None,
                }
                .encode_to_vec(),
            ),
            Some(&token),
        )
        .await;
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        // The ack payload is the (zero-byte) empty SendMessageResponse —
        // one length-prefixed frame with no content.
        assert_eq!(frames.len(), 1);
        assert!(SendMessageResponse::decode(frames[0]).is_ok());

        // Another session's token: none exists — an unknown token is refused.
        let resp = post(
            &url,
            "/flux.v1.ChatService/SendMessage",
            lp_frame(
                &SendMessageRequest {
                    chat_id,
                    message: "hi".into(),
                    interrupt: false,
                    client_msg_id: None,
                }
                .encode_to_vec(),
            ),
            Some("not-a-token"),
        )
        .await;
        assert_eq!(grpc_status_headers(&resp), Some(16));
        assert!(resp.bytes().await.unwrap().is_empty());
    }
}
