use flux_tools::{FileRead, FileWrite, ListDir};
use rig_core::tool::Tool;

#[tokio::test]
async fn file_read_write_and_list() {
    let tmp = tempfile::tempdir().unwrap();
    let workdir = tmp.path().to_path_buf();

    let writer = FileWrite::new(&workdir);
    writer
        .call(flux_tools::fs::FileWriteArgs {
            path: "hello.txt".to_string(),
            content: "world".to_string(),
        })
        .await
        .unwrap();

    let lister = ListDir::new(&workdir);
    let listing = lister
        .call(flux_tools::fs::ListDirArgs {
            path: ".".to_string(),
        })
        .await
        .unwrap();
    assert!(listing.contains("hello.txt"));

    let reader = FileRead::new(&workdir);
    let content = reader
        .call(flux_tools::fs::FileReadArgs {
            path: "hello.txt".to_string(),
        })
        .await
        .unwrap();
    assert_eq!(content, "world");
}

#[tokio::test]
async fn path_escape_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let workdir = tmp.path().to_path_buf();

    let reader = FileRead::new(&workdir);
    let result = reader
        .call(flux_tools::fs::FileReadArgs {
            path: "../etc/passwd".to_string(),
        })
        .await;
    assert!(result.is_err());
}
