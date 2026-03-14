//! Markdown-file-backed memory implementation.
//!
//! Stores memories as Markdown files on disk for OpenClaw compatibility:
//! - Daily conversation logs at `<base>/memory/YYYY-MM-DD.md`
//! - Long-term / persistent memory at `<base>/MEMORY.md`

use async_trait::async_trait;
use chrono::Utc;
use std::path::{Path, PathBuf};
use temm1e_core::error::Temm1eError;
use temm1e_core::{Memory, MemoryEntry, MemoryEntryType, SearchOpts};
use tokio::fs;
use tracing::{debug, info};

use crate::search::hybrid_search;

/// A memory backend that reads/writes plain Markdown files.
pub struct MarkdownMemory {
    /// Root directory under which `memory/` and `MEMORY.md` live.
    base_dir: PathBuf,
}

impl MarkdownMemory {
    /// Create a new MarkdownMemory rooted at `base_dir`.
    ///
    /// The directory structure is created lazily on the first write.
    pub async fn new(base_dir: impl Into<PathBuf>) -> Result<Self, Temm1eError> {
        let base_dir = base_dir.into();
        // Ensure the base directories exist.
        let memory_dir = base_dir.join("memory");
        fs::create_dir_all(&memory_dir).await?;
        info!(path = %base_dir.display(), "Markdown memory backend initialised");
        Ok(Self { base_dir })
    }

    // ----- path helpers ----------------------------------------------------

    fn daily_file(&self, date: &str) -> PathBuf {
        self.base_dir.join("memory").join(format!("{date}.md"))
    }

    fn long_term_file(&self) -> PathBuf {
        self.base_dir.join("MEMORY.md")
    }

    // ----- read / write helpers --------------------------------------------

    async fn read_file(path: &Path) -> Result<String, Temm1eError> {
        match fs::read_to_string(path).await {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(Temm1eError::Io(e)),
        }
    }

    async fn append_to_file(path: &Path, text: &str) -> Result<(), Temm1eError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let existing = Self::read_file(path).await?;
        let new_content = if existing.is_empty() {
            text.to_string()
        } else {
            format!("{existing}\n{text}")
        };
        fs::write(path, new_content).await?;
        Ok(())
    }

    /// Format a MemoryEntry as a Markdown section.
    fn entry_to_markdown(entry: &MemoryEntry) -> String {
        let ts = entry.timestamp.to_rfc3339();
        let session = entry.session_id.as_deref().unwrap_or("none");
        format!(
            "<!-- entry:{} session:{} type:{} -->\n### {} [{}]\n\n{}\n",
            entry.id,
            session,
            entry_type_to_str(&entry.entry_type),
            ts,
            entry_type_to_str(&entry.entry_type),
            entry.content,
        )
    }

    /// Parse all entries out of a Markdown file body.
    fn parse_entries(text: &str) -> Vec<MemoryEntry> {
        let mut entries = Vec::new();
        let blocks: Vec<&str> = text.split("<!-- entry:").collect();

        for block in blocks.iter().skip(1) {
            if let Some(entry) = Self::parse_single_block(block) {
                entries.push(entry);
            }
        }
        entries
    }

    fn parse_single_block(block: &str) -> Option<MemoryEntry> {
        // Expected prefix: `<id> session:<sid> type:<type> -->`
        let header_end = block.find("-->")?;
        let header = &block[..header_end].trim();

        // Parse the header tokens.
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 3 {
            return None;
        }
        let id = parts[0].to_string();
        let session_id = parts
            .iter()
            .find(|p| p.starts_with("session:"))
            .and_then(|p| p.strip_prefix("session:"))
            .and_then(|s| {
                if s == "none" {
                    None
                } else {
                    Some(s.to_string())
                }
            });
        let entry_type_str = parts
            .iter()
            .find(|p| p.starts_with("type:"))
            .and_then(|p| p.strip_prefix("type:"))?;
        let entry_type = str_to_entry_type(entry_type_str).ok()?;

        // After the `-->` we expect `\n### <timestamp> [<type>]\n\n<content>\n`
        let body = &block[header_end + 3..];

        // Extract timestamp from the ### line.
        let timestamp = body
            .lines()
            .find(|l| l.starts_with("### "))
            .and_then(|l| {
                let after = l.strip_prefix("### ")?;
                let ts_str = after.split(" [").next()?;
                chrono::DateTime::parse_from_rfc3339(ts_str.trim()).ok()
            })
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(Utc::now);

        // Content is everything after the first blank line following the ### line.
        let content = body
            .split_once("\n\n")
            .map(|x| x.1)
            .unwrap_or("")
            .trim()
            .to_string();

        Some(MemoryEntry {
            id,
            content,
            metadata: serde_json::json!({}),
            timestamp,
            session_id,
            entry_type,
        })
    }

    /// Collect all Markdown files in the memory directory.
    async fn all_memory_files(&self) -> Result<Vec<PathBuf>, Temm1eError> {
        let mut files = Vec::new();

        // Daily files
        let memory_dir = self.base_dir.join("memory");
        if memory_dir.exists() {
            let mut dir = fs::read_dir(&memory_dir).await?;
            while let Some(entry) = dir.next_entry().await? {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "md") {
                    files.push(path);
                }
            }
        }

        // Long-term file
        let lt = self.long_term_file();
        if lt.exists() {
            files.push(lt);
        }

        Ok(files)
    }

    /// Read and parse all entries from all files.
    async fn all_entries(&self) -> Result<Vec<MemoryEntry>, Temm1eError> {
        let files = self.all_memory_files().await?;
        let mut entries = Vec::new();
        for f in files {
            let text = Self::read_file(&f).await?;
            entries.extend(Self::parse_entries(&text));
        }
        // Sort by timestamp ascending.
        entries.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        Ok(entries)
    }
}

#[async_trait]
impl Memory for MarkdownMemory {
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError> {
        let md = Self::entry_to_markdown(&entry);

        match entry.entry_type {
            MemoryEntryType::LongTerm => {
                let path = self.long_term_file();
                Self::append_to_file(&path, &md).await?;
            }
            _ => {
                let date = entry.timestamp.format("%Y-%m-%d").to_string();
                let path = self.daily_file(&date);
                Self::append_to_file(&path, &md).await?;
            }
        }

        debug!(id = %entry.id, "Stored markdown memory entry");
        Ok(())
    }

    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>, Temm1eError> {
        let mut entries = self.all_entries().await?;

        // Apply filters.
        if let Some(ref session) = opts.session_filter {
            entries.retain(|e| e.session_id.as_deref() == Some(session.as_str()));
        }
        if let Some(ref et) = opts.entry_type_filter {
            let et_str = entry_type_to_str(et);
            entries.retain(|e| entry_type_to_str(&e.entry_type) == et_str);
        }

        let results = hybrid_search(query, &entries, opts.vector_weight, opts.keyword_weight);
        Ok(results.into_iter().take(opts.limit).collect())
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError> {
        let entries = self.all_entries().await?;
        Ok(entries.into_iter().find(|e| e.id == id))
    }

    async fn delete(&self, id: &str) -> Result<(), Temm1eError> {
        // To delete we must rewrite the files without the target entry.
        let files = self.all_memory_files().await?;
        for f in files {
            let text = Self::read_file(&f).await?;
            if text.contains(&format!("<!-- entry:{id} ")) {
                let entries: Vec<MemoryEntry> = Self::parse_entries(&text)
                    .into_iter()
                    .filter(|e| e.id != id)
                    .collect();
                let new_text: String = entries
                    .iter()
                    .map(Self::entry_to_markdown)
                    .collect::<Vec<_>>()
                    .join("\n");
                fs::write(&f, new_text).await?;
                debug!(id = %id, file = %f.display(), "Deleted markdown memory entry");
                return Ok(());
            }
        }
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError> {
        let entries = self.all_entries().await?;
        let mut sessions: Vec<String> = entries
            .into_iter()
            .filter_map(|e| e.session_id)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        sessions.sort();
        Ok(sessions)
    }

    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError> {
        let entries = self.all_entries().await?;
        let history: Vec<MemoryEntry> = entries
            .into_iter()
            .filter(|e| e.session_id.as_deref() == Some(session_id))
            .take(limit)
            .collect();
        Ok(history)
    }

    fn backend_name(&self) -> &str {
        "markdown"
    }
}

// ---------------------------------------------------------------------------
// Shared helpers (same mapping as sqlite.rs)
// ---------------------------------------------------------------------------

fn entry_type_to_str(et: &MemoryEntryType) -> &'static str {
    match et {
        MemoryEntryType::Conversation => "conversation",
        MemoryEntryType::LongTerm => "long_term",
        MemoryEntryType::DailyLog => "daily_log",
        MemoryEntryType::Skill => "skill",
        MemoryEntryType::Knowledge => "knowledge",
        MemoryEntryType::Blueprint => "blueprint",
    }
}

fn str_to_entry_type(s: &str) -> Result<MemoryEntryType, Temm1eError> {
    match s {
        "conversation" => Ok(MemoryEntryType::Conversation),
        "long_term" => Ok(MemoryEntryType::LongTerm),
        "daily_log" => Ok(MemoryEntryType::DailyLog),
        "skill" => Ok(MemoryEntryType::Skill),
        "knowledge" => Ok(MemoryEntryType::Knowledge),
        "blueprint" => Ok(MemoryEntryType::Blueprint),
        other => Err(Temm1eError::Memory(format!("Unknown entry type: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_entry(id: &str, content: &str, et: MemoryEntryType) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            metadata: serde_json::json!({}),
            timestamp: Utc::now(),
            session_id: Some("test-session".to_string()),
            entry_type: et,
        }
    }

    #[tokio::test]
    async fn store_and_get_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = MarkdownMemory::new(tmp.path()).await.unwrap();

        let entry = make_entry("md1", "Hello from markdown", MemoryEntryType::Conversation);
        mem.store(entry).await.unwrap();

        let fetched = mem.get("md1").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().content, "Hello from markdown");
    }

    #[tokio::test]
    async fn store_long_term_in_memory_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = MarkdownMemory::new(tmp.path()).await.unwrap();

        let entry = make_entry("lt1", "Important fact", MemoryEntryType::LongTerm);
        mem.store(entry).await.unwrap();

        // Check the MEMORY.md file exists
        let lt_path = tmp.path().join("MEMORY.md");
        assert!(lt_path.exists());

        let content = tokio::fs::read_to_string(&lt_path).await.unwrap();
        assert!(content.contains("Important fact"));
        assert!(content.contains("<!-- entry:lt1"));
    }

    #[tokio::test]
    async fn delete_entry_from_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = MarkdownMemory::new(tmp.path()).await.unwrap();

        mem.store(make_entry(
            "del1",
            "to delete",
            MemoryEntryType::Conversation,
        ))
        .await
        .unwrap();
        mem.store(make_entry(
            "keep1",
            "to keep",
            MemoryEntryType::Conversation,
        ))
        .await
        .unwrap();

        mem.delete("del1").await.unwrap();

        assert!(mem.get("del1").await.unwrap().is_none());
        assert!(mem.get("keep1").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn list_sessions_from_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let mem = MarkdownMemory::new(tmp.path()).await.unwrap();

        let mut e1 = make_entry("s1", "a", MemoryEntryType::Conversation);
        e1.session_id = Some("session_a".to_string());
        mem.store(e1).await.unwrap();

        let mut e2 = make_entry("s2", "b", MemoryEntryType::Conversation);
        e2.session_id = Some("session_b".to_string());
        mem.store(e2).await.unwrap();

        let sessions = mem.list_sessions().await.unwrap();
        assert!(sessions.contains(&"session_a".to_string()));
        assert!(sessions.contains(&"session_b".to_string()));
    }

    #[test]
    fn entry_to_markdown_format() {
        let entry = MemoryEntry {
            id: "fmt1".to_string(),
            content: "Test content".to_string(),
            metadata: serde_json::json!({}),
            timestamp: chrono::DateTime::parse_from_rfc3339("2025-01-15T10:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            session_id: Some("sess1".to_string()),
            entry_type: MemoryEntryType::Conversation,
        };

        let md = MarkdownMemory::entry_to_markdown(&entry);
        assert!(md.contains("<!-- entry:fmt1 session:sess1 type:conversation -->"));
        assert!(md.contains("### 2025-01-15"));
        assert!(md.contains("Test content"));
    }

    #[test]
    fn parse_entries_roundtrip() {
        let entry = MemoryEntry {
            id: "rt1".to_string(),
            content: "Roundtrip test".to_string(),
            metadata: serde_json::json!({}),
            timestamp: chrono::DateTime::parse_from_rfc3339("2025-06-01T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
            session_id: Some("s1".to_string()),
            entry_type: MemoryEntryType::DailyLog,
        };

        let md = MarkdownMemory::entry_to_markdown(&entry);
        let parsed = MarkdownMemory::parse_entries(&md);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "rt1");
        assert_eq!(parsed[0].content, "Roundtrip test");
        assert_eq!(parsed[0].session_id.as_deref(), Some("s1"));
    }
}
