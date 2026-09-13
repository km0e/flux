//! The fs family — `FsList` / `FsRead`, pure reuse of the browsing
//! helpers in crate::fsbrowse.
//!
//! Browse failures ride the response's inline `error` (D4') — the picker
//! renders them; the gRPC status stays reserved for transport faults.

use crate::fsbrowse;
use crate::fsbrowse::{FsEntryKind as WsEntryKind, GitStatus as WsGitStatus};
use flux_proto::flux::v1::file_system_service_server::FileSystemService;
use flux_proto::flux::v1::{
    FsEntry, FsEntryKind, FsListRequest, FsListResponse, FsReadRequest, FsReadResponse, GitStatus,
};
use tonic::Request;

#[derive(Default)]
pub(crate) struct FsService;

#[async_trait::async_trait]
impl FileSystemService for FsService {
    async fn fs_list(
        &self,
        request: Request<FsListRequest>,
    ) -> Result<tonic::Response<FsListResponse>, tonic::Status> {
        let req = request.into_inner();
        let requested = req.path.clone().unwrap_or_default();
        // list_dir spawns git subprocesses and stats every entry — blocking
        // work that must NOT sit on a tokio worker: the UI's fixed-cadence
        // auto-refresh makes this a high-frequency call, and the chat
        // stream pump shares the runtime. A panicking blocking task
        // degrades to the same inline error the UI renders.
        let path = req.path;
        let listing = tokio::task::spawn_blocking(move || fsbrowse::list_dir(path.as_deref()))
            .await
            .unwrap_or_else(|e| fsbrowse::Listing::Err(format!("browse failed: {e}")));
        let response = match listing {
            fsbrowse::Listing::Ok {
                path,
                parent,
                entries,
            } => FsListResponse {
                requested,
                error: None,
                path: Some(path),
                parent,
                entries: entries.into_iter().map(proto_entry).collect(),
            },
            fsbrowse::Listing::Err(e) => FsListResponse {
                requested,
                error: Some(e),
                path: None,
                parent: None,
                entries: Vec::new(),
            },
        };
        Ok(tonic::Response::new(response))
    }

    async fn fs_read(
        &self,
        request: Request<FsReadRequest>,
    ) -> Result<tonic::Response<FsReadResponse>, tonic::Status> {
        let req = request.into_inner();
        let requested = req.path.clone();
        // Same inline-error contract as fs_list, same spawn_blocking
        // rationale (synchronous file I/O off the async workers).
        let path = req.path;
        let preview = tokio::task::spawn_blocking(move || fsbrowse::preview_file(&path))
            .await
            .unwrap_or_else(|e| Err(format!("preview failed: {e}")));
        // Same inline-error contract as fs_list: preview failures are UI
        // data (the pane renders a neutral placeholder), never statuses.
        let response = match preview {
            Ok(p) => FsReadResponse {
                requested,
                error: None,
                content: Some(p.content),
                truncated: Some(p.truncated),
                size: Some(p.size),
            },
            Err(e) => FsReadResponse {
                requested,
                error: Some(e),
                content: None,
                truncated: None,
                size: None,
            },
        };
        Ok(tonic::Response::new(response))
    }
}

// Free functions, not `From` impls: both sides are foreign types (orphan
// rule) — the mapping is the explicit contract-translation seam.
fn proto_entry(e: crate::fsbrowse::FsEntry) -> FsEntry {
    FsEntry {
        name: e.name,
        kind: match e.kind {
            WsEntryKind::Dir => FsEntryKind::Dir,
            WsEntryKind::File => FsEntryKind::File,
        }
        .into(),
        size: e.size,
        git: e.git.map(|g| proto_git(g).into()),
    }
}

fn proto_git(g: WsGitStatus) -> GitStatus {
    match g {
        WsGitStatus::Modified => GitStatus::Modified,
        WsGitStatus::Added => GitStatus::Added,
        WsGitStatus::Untracked => GitStatus::Untracked,
        WsGitStatus::Conflicted => GitStatus::Conflicted,
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::*;
    use super::*;
    use flux_proto::flux::v1::{FsListRequest, FsListResponse, FsReadRequest, FsReadResponse};
    use flux_proto::prost::Message as _;

    #[tokio::test]
    async fn fs_list_round_trips_over_grpc_web_http1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();

        let (_state, url) = fixture().await;
        let req = FsListRequest {
            path: Some(dir.path().to_str().unwrap().to_owned()),
        };
        let resp = web_client()
            .post(format!("{url}/flux.v1.FileSystemService/FsList"))
            .header("content-type", "application/grpc-web+proto")
            .header("x-grpc-web", "1")
            .body(lp_frame(&req.encode_to_vec()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0), "grpc-status ok");
        let out = FsListResponse::decode(frames[0]).unwrap();
        assert_eq!(out.requested, dir.path().to_str().unwrap());
        assert_eq!(
            out.path.as_deref(),
            dir.path().canonicalize().unwrap().to_str()
        );
        // Directories first, each name-sorted.
        let names: Vec<&str> = out.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["sub", "a.txt"]);
        assert_eq!(out.entries[0].kind, FsEntryKind::Dir as i32);
        assert_eq!(out.entries[1].kind, FsEntryKind::File as i32);
        assert_eq!(out.entries[1].size, Some(5));
    }

    #[tokio::test]
    async fn fs_read_round_trips_over_grpc_web_http1() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("note.txt"), "hello preview").unwrap();

        let (_state, url) = fixture().await;
        let req = FsReadRequest {
            path: dir.path().join("note.txt").to_str().unwrap().to_owned(),
        };
        let resp = web_client()
            .post(format!("{url}/flux.v1.FileSystemService/FsRead"))
            .header("content-type", "application/grpc-web+proto")
            .body(lp_frame(&req.encode_to_vec()))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 200);
        let bytes = resp.bytes().await.unwrap();
        let (frames, trailer) = parse_frames(&bytes);
        assert_eq!(trailer_grpc_status(trailer), Some(0));
        let out = FsReadResponse::decode(frames[0]).unwrap();
        assert!(out.error.is_none());
        assert_eq!(out.content.as_deref(), Some("hello preview"));
        assert_eq!(out.truncated, Some(false));
        assert_eq!(out.size, Some(13));
    }
}
