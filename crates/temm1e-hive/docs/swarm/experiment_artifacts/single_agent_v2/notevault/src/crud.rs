use crate::db::Database;
use crate::error::{NoteVaultError, Result};
use crate::models::{CreateNoteRequest, Note, NoteFilter};
use chrono::Utc;
use uuid::Uuid;

pub async fn create_note(db: &Database, req: CreateNoteRequest) -> Result<Note> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let note = Note {
        id: id.clone(),
        title: req.title,
        body: req.body,
        created_at: now.clone(),
        updated_at: now,
    };

    sqlx::query(
        r#"
        INSERT INTO notes (id, title, body, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?)
        "#,
    )
    .bind(&note.id)
    .bind(&note.title)
    .bind(&note.body)
    .bind(&note.created_at)
    .bind(&note.updated_at)
    .execute(db.pool())
    .await?;

    Ok(note)
}

pub async fn get_note(db: &Database, id: &str) -> Result<Note> {
    let note = sqlx::query_as::<_, Note>(
        r#"
        SELECT id, title, body, created_at, updated_at
        FROM notes
        WHERE id = ?
        "#,
    )
    .bind(id)
    .fetch_optional(db.pool())
    .await?;

    note.ok_or_else(|| NoteVaultError::NotFound(format!("Note with id {} not found", id)))
}

pub async fn list_notes(db: &Database, filter: NoteFilter) -> Result<Vec<Note>> {
    let notes = if let Some(search) = filter.search {
        let search_pattern = format!("%{}%", search);
        sqlx::query_as::<_, Note>(
            r#"
            SELECT id, title, body, created_at, updated_at
            FROM notes
            WHERE title LIKE ? OR body LIKE ?
            ORDER BY created_at DESC
            "#,
        )
        .bind(&search_pattern)
        .bind(&search_pattern)
        .fetch_all(db.pool())
        .await?
    } else {
        sqlx::query_as::<_, Note>(
            r#"
            SELECT id, title, body, created_at, updated_at
            FROM notes
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(db.pool())
        .await?
    };

    Ok(notes)
}

pub async fn update_note(db: &Database, id: &str, req: CreateNoteRequest) -> Result<Note> {
    let now = Utc::now().to_rfc3339();

    let rows_affected = sqlx::query(
        r#"
        UPDATE notes
        SET title = ?, body = ?, updated_at = ?
        WHERE id = ?
        "#,
    )
    .bind(&req.title)
    .bind(&req.body)
    .bind(&now)
    .bind(id)
    .execute(db.pool())
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(NoteVaultError::NotFound(format!("Note with id {} not found", id)));
    }

    get_note(db, id).await
}

pub async fn delete_note(db: &Database, id: &str) -> Result<()> {
    let rows_affected = sqlx::query(
        r#"
        DELETE FROM notes
        WHERE id = ?
        "#,
    )
    .bind(id)
    .execute(db.pool())
    .await?
    .rows_affected();

    if rows_affected == 0 {
        return Err(NoteVaultError::NotFound(format!("Note with id {} not found", id)));
    }

    Ok(())
}