//! Integration tests for memory backends — tests the factory function
//! and cross-backend behavior consistency.

use temm1e_core::{MemoryEntry, MemoryEntryType, SearchOpts};
use temm1e_memory::create_memory_backend;

fn make_entry(id: &str, content: &str, session: Option<&str>, et: MemoryEntryType) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        content: content.to_string(),
        metadata: serde_json::json!({"source": "integration_test"}),
        timestamp: chrono::Utc::now(),
        session_id: session.map(String::from),
        entry_type: et,
    }
}

#[tokio::test]
async fn create_sqlite_backend() {
    let backend = create_memory_backend("sqlite", "sqlite::memory:")
        .await
        .unwrap();
    assert_eq!(backend.backend_name(), "sqlite");
}

#[tokio::test]
async fn create_markdown_backend() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = create_memory_backend("markdown", tmp.path().to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(backend.backend_name(), "markdown");
}

#[tokio::test]
async fn create_unknown_backend_fails() {
    let result = create_memory_backend("redis", "redis://localhost").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn sqlite_full_lifecycle() {
    let mem = create_memory_backend("sqlite", "sqlite::memory:")
        .await
        .unwrap();

    // Store
    mem.store(make_entry(
        "lc1",
        "lifecycle test",
        Some("sess1"),
        MemoryEntryType::Conversation,
    ))
    .await
    .unwrap();

    // Get
    let entry = mem.get("lc1").await.unwrap().unwrap();
    assert_eq!(entry.content, "lifecycle test");
    assert_eq!(entry.session_id.as_deref(), Some("sess1"));

    // Search
    let results = mem
        .search("lifecycle", SearchOpts::default())
        .await
        .unwrap();
    assert_eq!(results.len(), 1);

    // List sessions
    let sessions = mem.list_sessions().await.unwrap();
    assert_eq!(sessions, vec!["sess1"]);

    // Session history
    let history = mem.get_session_history("sess1", 10).await.unwrap();
    assert_eq!(history.len(), 1);

    // Delete
    mem.delete("lc1").await.unwrap();
    assert!(mem.get("lc1").await.unwrap().is_none());
}

#[tokio::test]
async fn markdown_full_lifecycle() {
    let tmp = tempfile::tempdir().unwrap();
    let mem = create_memory_backend("markdown", tmp.path().to_str().unwrap())
        .await
        .unwrap();

    // Store
    mem.store(make_entry(
        "mlc1",
        "markdown lifecycle",
        Some("ms1"),
        MemoryEntryType::Conversation,
    ))
    .await
    .unwrap();

    // Get
    let entry = mem.get("mlc1").await.unwrap().unwrap();
    assert_eq!(entry.content, "markdown lifecycle");

    // Search
    let results = mem.search("markdown", SearchOpts::default()).await.unwrap();
    assert_eq!(results.len(), 1);

    // List sessions
    let sessions = mem.list_sessions().await.unwrap();
    assert!(sessions.contains(&"ms1".to_string()));

    // Delete
    mem.delete("mlc1").await.unwrap();
    assert!(mem.get("mlc1").await.unwrap().is_none());
}

#[tokio::test]
async fn sqlite_search_with_unicode() {
    let mem = create_memory_backend("sqlite", "sqlite::memory:")
        .await
        .unwrap();

    mem.store(make_entry(
        "u1",
        "\u{1F600} emoji content \u{2764}",
        None,
        MemoryEntryType::Conversation,
    ))
    .await
    .unwrap();
    mem.store(make_entry(
        "u2",
        "\u{4F60}\u{597D}\u{4E16}\u{754C}",
        None,
        MemoryEntryType::Conversation,
    ))
    .await
    .unwrap();

    let results = mem
        .search("\u{4F60}\u{597D}", SearchOpts::default())
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].id, "u2");
}

#[tokio::test]
async fn sqlite_mixed_entry_types() {
    let mem = create_memory_backend("sqlite", "sqlite::memory:")
        .await
        .unwrap();

    mem.store(make_entry(
        "mix1",
        "convo",
        Some("s"),
        MemoryEntryType::Conversation,
    ))
    .await
    .unwrap();
    mem.store(make_entry(
        "mix2",
        "long term fact",
        None,
        MemoryEntryType::LongTerm,
    ))
    .await
    .unwrap();
    mem.store(make_entry(
        "mix3",
        "daily log",
        None,
        MemoryEntryType::DailyLog,
    ))
    .await
    .unwrap();
    mem.store(make_entry(
        "mix4",
        "skill definition",
        None,
        MemoryEntryType::Skill,
    ))
    .await
    .unwrap();

    // All should be retrievable
    assert!(mem.get("mix1").await.unwrap().is_some());
    assert!(mem.get("mix2").await.unwrap().is_some());
    assert!(mem.get("mix3").await.unwrap().is_some());
    assert!(mem.get("mix4").await.unwrap().is_some());
}
