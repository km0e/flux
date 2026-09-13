//! Codegen: the `.proto` single source of truth lives at the repo root
//! (`proto/`), compiled by tonic-prost-build/prost-build into this crate at
//! build time. `protoc` must be on PATH (system package, no vendoring).
//!
//! Both sides are generated: the server types power the axum-mounted
//! services; the client types power the Rust e2e (tonic speaks native
//! gRPC/h2c, which hyper-util's auto detection serves alongside HTTP/1.1
//! on the same listener — the browser's gRPC-Web path stays pinned by the
//! headless-Chrome e2e instead).
//!
//! tonic 0.14 moved the prost codegen out of `tonic-build` into
//! `tonic-prost-build` (the generated code pairs tonic's generic service
//! plumbing with `tonic_prost::ProstCodec`).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../proto");
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(
            &[
                proto_root.join("flux/v1/common.proto"),
                proto_root.join("flux/v1/fs.proto"),
                proto_root.join("flux/v1/providers.proto"),
                proto_root.join("flux/v1/models.proto"),
                proto_root.join("flux/v1/mcp.proto"),
                proto_root.join("flux/v1/skills.proto"),
                proto_root.join("flux/v1/chats.proto"),
                proto_root.join("flux/v1/events.proto"),
            ],
            std::slice::from_ref(&proto_root),
        )?;
    // Recursively rebuild when any proto source changes.
    println!("cargo:rerun-if-changed={}", proto_root.display());
    Ok(())
}
