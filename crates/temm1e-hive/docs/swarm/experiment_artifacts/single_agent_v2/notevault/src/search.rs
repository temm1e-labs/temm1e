use crate::db::Database;
use crate::error::Result;
use crate::models::{Note, NoteFilter};

pub async fn search_notes(db: &Database, filter: &NoteFilter) -> Result<Vec<Note>> {
    let pool = db.pool();

    if let Some(ref search) = filter.search {
        let pattern = format!("%{}%", search);
        let notes = sqlx::query_as::<_, Note>(
            r#"
            SELECT id, title, body, created_at, updated_at
            FROM notes
            WHERE title LIKE $1 OR body LIKE $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(pattern)
        .fetch_all(pool)
        .await?;
        
        Ok(notes)
    } else {
        let notes = sqlx::query_as::<_, Note>(
            r#"
            SELECT id, title, body, created_at, updated_at
            FROM notes
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(pool)
        .await?;
        
        Ok(notes)
    }
}