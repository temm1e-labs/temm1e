use crate::db::Database;
use crate::models::{Note, NoteFilter};

pub async fn search_notes(db: &Database, filter: &NoteFilter) -> Result<Vec<Note>, sqlx::Error> {
    if let Some(search_term) = &filter.search {
        let wildcard = format!("%{}%", search_term);
        sqlx::query_as::<_, Note>(
            r#"
            SELECT id, title, body, created_at, updated_at 
            FROM notes 
            WHERE title LIKE ? OR body LIKE ? 
            ORDER BY created_at DESC
            "#,
        )
        .bind(&wildcard)
        .bind(&wildcard)
        .fetch_all(db.pool())
        .await
    } else {
        sqlx::query_as::<_, Note>(
            r#"
            SELECT id, title, body, created_at, updated_at 
            FROM notes 
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(db.pool())
        .await
    }
}