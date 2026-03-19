use crate::db::Database;
use crate::models::{CreateNoteRequest, Note};
use chrono::Utc;
use uuid::Uuid;

pub async fn create_note(db: &Database, req: CreateNoteRequest) -> Result<Note, sqlx::Error> {
    let id = Uuid::new_v4().to_string();
    let now = Utc::now().to_rfc3339();

    let note = Note {
        id: id.clone(),
        title: req.title.clone(),
        body: req.body.clone(),
        created_at: now.clone(),
        updated_at: now.clone(),
    };

    sqlx::query(
        "INSERT INTO notes (id, title, body, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
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

pub async fn get_note(db: &Database, id: &str) -> Result<Note, sqlx::Error> {
    sqlx::query_as::<_, Note>("SELECT * FROM notes WHERE id = ?")
        .bind(id)
        .fetch_one(db.pool())
        .await
}