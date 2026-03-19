use notevault::*;

async fn setup_db() -> Database {
    Database::new("sqlite::memory:")
        .await
        .expect("Failed to create in-memory database")
}

#[tokio::test]
async fn test_create_and_get() {
    let db = setup_db().await;
    let req = CreateNoteRequest {
        title: "Test Note".into(),
        body: "This is a test note.".into(),
    };

    let created = create_note(&db, req).await.expect("Failed to create note");
    assert_eq!(created.title, "Test Note");
    assert_eq!(created.body, "This is a test note.");
    assert!(!created.id.is_empty());

    let fetched = get_note(&db, &created.id)
        .await
        .expect("Failed to get note");
    assert_eq!(fetched.id, created.id);
    assert_eq!(fetched.title, "Test Note");
    assert_eq!(fetched.body, "This is a test note.");
}

#[tokio::test]
async fn test_list() {
    let db = setup_db().await;
    let req1 = CreateNoteRequest {
        title: "Note 1".into(),
        body: "Body 1".into(),
    };
    create_note(&db, req1).await.expect("Failed to create note 1");

    let req2 = CreateNoteRequest {
        title: "Note 2".into(),
        body: "Body 2".into(),
    };
    create_note(&db, req2).await.expect("Failed to create note 2");

    let filter = NoteFilter { search: None };
    let notes = search_notes(&db, &filter).await.expect("Failed to search notes");
    assert_eq!(notes.len(), 2);
}

#[tokio::test]
async fn test_search() {
    let db = setup_db().await;
    let req1 = CreateNoteRequest {
        title: "Apple".into(),
        body: "A fruit".into(),
    };
    create_note(&db, req1).await.expect("Failed to create note 1");

    let req2 = CreateNoteRequest {
        title: "Banana".into(),
        body: "Another fruit".into(),
    };
    create_note(&db, req2).await.expect("Failed to create note 2");

    let filter = NoteFilter { search: Some("Apple".into()) };
    let notes = search_notes(&db, &filter).await.expect("Failed to search notes");
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].title, "Apple");
}