#[cfg(test)]
mod probe {
    #[tokio::test]
    async fn dbg_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(flux_store::Store::open_in_memory().await.unwrap());
        store.insert_chat("c", "t").await.unwrap();
        let text = crate::build_scaffold_text(
            store,
            "c",
            dir.path().to_str().unwrap(),
            &crate::ScaffoldConfig::default(),
        )
        .await
        .unwrap();
        eprintln!("SCAFFOLD LEN: {}", text.chars().count());
        eprintln!(
            "HEAD: {}",
            text.chars()
                .take(300)
                .collect::<String>()
                .replace('\n', " | ")
        );
    }
}
