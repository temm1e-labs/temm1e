# Skill: Add a new memory backend to TEMM1E

## When to use

Use this skill when the user asks to add a new memory/storage backend (e.g., PostgreSQL, Redis, DynamoDB, Qdrant, ChromaDB, Pinecone) to TEMM1E.

## Reference implementation

Study the existing backends:
- `crates/temm1e-memory/src/sqlite.rs` -- full Memory trait implementation with SQLite via sqlx
- `crates/temm1e-memory/src/markdown.rs` -- file-based Memory implementation for OpenClaw compatibility
- `crates/temm1e-core/src/traits/memory.rs` -- the `Memory` trait definition plus `MemoryEntry`, `SearchOpts`, `MemoryEntryType`

## Steps

### 1. Create the backend source file

Create `crates/temm1e-memory/src/<backend_name>.rs` using the template below.

### 2. Add the module to lib.rs

Edit `crates/temm1e-memory/src/lib.rs`:
- Add `pub mod <backend_name>;`
- Add `pub use <backend_name>::<BackendName>Memory;`
- Add a match arm in `create_memory_backend()` for the new backend name

### 3. Add dependencies

Edit `crates/temm1e-memory/Cargo.toml`:
- Add the backend's client library as a dependency
- If it should be feature-gated, add it as optional and create a feature flag

If feature-gated, also edit root `Cargo.toml`:
- Add the feature flag: `<backend_name> = ["temm1e-memory/<backend_name>"]`

### 4. Write tests

Include tests in the backend source file:
- Test `store()` and `get()` roundtrip
- Test `get()` for nonexistent ID returns `None`
- Test `delete()` removes entry
- Test `search()` with keyword matching
- Test `search()` with session filter
- Test `search()` with entry type filter
- Test `list_sessions()` returns distinct sessions
- Test `get_session_history()` ordering and limit
- Test `store()` with duplicate ID replaces entry
- Test `backend_name()` returns correct string
- Test empty database edge cases

### 5. Verify

```bash
cargo check -p temm1e-memory
cargo test -p temm1e-memory
cargo clippy -p temm1e-memory -- -D warnings
```

## Template

```rust
//! <BackendName>-backed memory implementation.

use async_trait::async_trait;
use temm1e_core::{Memory, MemoryEntry, MemoryEntryType, SearchOpts};
use temm1e_core::error::Temm1eError;
use tracing::{debug, info};

/// A memory backend backed by <BackendName>.
pub struct <BackendName>Memory {
    // TODO: Add connection pool or client handle
    // e.g., pool: PgPool, client: redis::Client, etc.
}

impl <BackendName>Memory {
    /// Create a new <BackendName>Memory and initialize the schema.
    ///
    /// `connection_url` is the connection string for the backend.
    pub async fn new(connection_url: &str) -> Result<Self, Temm1eError> {
        // TODO: Establish connection, create tables/collections if needed
        info!("<BackendName> memory backend initialised");
        todo!("Implement connection setup")
    }
}

#[async_trait]
impl Memory for <BackendName>Memory {
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError> {
        // TODO: Upsert the entry (INSERT OR REPLACE / PUT)
        // Fields to persist:
        //   entry.id          -- primary key (String)
        //   entry.content     -- the text content (String)
        //   entry.metadata    -- arbitrary JSON (serde_json::Value)
        //   entry.timestamp   -- chrono::DateTime<Utc>, store as RFC 3339
        //   entry.session_id  -- optional session grouping (Option<String>)
        //   entry.entry_type  -- enum: Conversation | LongTerm | DailyLog | Skill
        debug!(id = %entry.id, "Stored memory entry");
        todo!("Implement store")
    }

    async fn search(
        &self,
        query: &str,
        opts: SearchOpts,
    ) -> Result<Vec<MemoryEntry>, Temm1eError> {
        // TODO: Implement search
        // - Keyword matching on content (at minimum, LIKE '%query%')
        // - Apply opts.session_filter if set
        // - Apply opts.entry_type_filter if set
        // - Respect opts.limit
        // - Order by timestamp DESC (most recent first)
        // - For vector backends: use opts.vector_weight and opts.keyword_weight
        //   for hybrid scoring via crate::search::hybrid_search
        todo!("Implement search")
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError> {
        // TODO: Fetch by primary key, return None if not found
        todo!("Implement get")
    }

    async fn delete(&self, id: &str) -> Result<(), Temm1eError> {
        // TODO: Delete by primary key (no error if not found)
        debug!(id = %id, "Deleted memory entry");
        todo!("Implement delete")
    }

    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError> {
        // TODO: SELECT DISTINCT session_id WHERE session_id IS NOT NULL
        todo!("Implement list_sessions")
    }

    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError> {
        // TODO: Fetch entries for session, ORDER BY timestamp ASC, LIMIT
        todo!("Implement get_session_history")
    }

    fn backend_name(&self) -> &str {
        "<backend_name>"
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn entry_type_to_str(et: &MemoryEntryType) -> &'static str {
    match et {
        MemoryEntryType::Conversation => "conversation",
        MemoryEntryType::LongTerm => "long_term",
        MemoryEntryType::DailyLog => "daily_log",
        MemoryEntryType::Skill => "skill",
    }
}

fn str_to_entry_type(s: &str) -> Result<MemoryEntryType, Temm1eError> {
    match s {
        "conversation" => Ok(MemoryEntryType::Conversation),
        "long_term" => Ok(MemoryEntryType::LongTerm),
        "daily_log" => Ok(MemoryEntryType::DailyLog),
        "skill" => Ok(MemoryEntryType::Skill),
        other => Err(Temm1eError::Memory(format!(
            "Unknown entry type: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entry(id: &str, content: &str, session: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            metadata: serde_json::json!({"source": "test"}),
            timestamp: Utc::now(),
            session_id: session.map(String::from),
            entry_type: MemoryEntryType::Conversation,
        }
    }

    #[tokio::test]
    async fn store_and_get() {
        // TODO: Create backend with test connection
        // let mem = <BackendName>Memory::new("test-url").await.unwrap();
        // let entry = make_entry("e1", "hello world", None);
        // mem.store(entry).await.unwrap();
        // let fetched = mem.get("e1").await.unwrap();
        // assert!(fetched.is_some());
        // assert_eq!(fetched.unwrap().content, "hello world");
    }

    #[tokio::test]
    async fn get_nonexistent_returns_none() {
        // let mem = <BackendName>Memory::new("test-url").await.unwrap();
        // assert!(mem.get("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_entry() {
        // let mem = <BackendName>Memory::new("test-url").await.unwrap();
        // mem.store(make_entry("d1", "to delete", None)).await.unwrap();
        // mem.delete("d1").await.unwrap();
        // assert!(mem.get("d1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn search_by_keyword() {
        // let mem = <BackendName>Memory::new("test-url").await.unwrap();
        // mem.store(make_entry("s1", "Rust programming", None)).await.unwrap();
        // mem.store(make_entry("s2", "Python scripting", None)).await.unwrap();
        // let results = mem.search("Rust", SearchOpts::default()).await.unwrap();
        // assert_eq!(results.len(), 1);
    }

    #[test]
    fn backend_name_is_correct() {
        // assert_eq!(<BackendName>Memory { .. }.backend_name(), "<backend_name>");
    }

    #[test]
    fn entry_type_roundtrip() {
        let types = vec![
            MemoryEntryType::Conversation,
            MemoryEntryType::LongTerm,
            MemoryEntryType::DailyLog,
            MemoryEntryType::Skill,
        ];
        for et in types {
            let s = entry_type_to_str(&et);
            let restored = str_to_entry_type(s).unwrap();
            assert_eq!(entry_type_to_str(&restored), s);
        }
    }
}
```

## Key conventions

- **Error types**: Use `Temm1eError::Memory(...)` for all memory backend errors.
- **Upsert semantics**: `store()` must replace existing entries with the same ID (INSERT OR REPLACE behavior).
- **Delete idempotency**: `delete()` must not error if the entry does not exist.
- **Timestamp format**: Store as RFC 3339 string for portability.
- **Entry type serialization**: Use the `entry_type_to_str` / `str_to_entry_type` helpers for consistent mapping.
- **Search**: At minimum, support keyword matching. For vector backends, integrate with `crate::search::hybrid_search` using `opts.vector_weight` and `opts.keyword_weight`.
- **Feature gating**: If the backend requires a heavy dependency (e.g., PostgreSQL driver), put it behind a feature flag.
- **No cross-impl deps**: Memory backends must not depend on each other.
