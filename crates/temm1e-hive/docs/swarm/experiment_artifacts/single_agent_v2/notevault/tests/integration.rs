use notevault::*;

#[tokio::test]
async fn test_create_and_get_note() {
    let db = Database::new("sqlite::memory:").await.unwrap();
    
    let req = CreateNoteRequest {
        title: "My First Note".to_string(),
        body: "This is the body of the note".to_string(),
    };
    
    let created_note = create_note(&db, req).await.unwrap();
    assert_eq!(created_note.title, "My First Note");
    assert_eq!(created_note.body, "This is the body of the note");
    assert!(!created_note.id.is_empty());
    assert!(!created_note.created_at.is_empty());
    assert!(!created_note.updated_at.is_empty());

    let fetched_note = get_note(&db, &created_note.id).await.unwrap();
    assert_eq!(fetched_note.id, created_note.id);
    assert_eq!(fetched_note.title, "My First Note");
    assert_eq!(fetched_note.body, "This is the body of the note");
}

#[tokio::test]
async fn test_list_notes() {
    let db = Database::new("sqlite::memory:").await.unwrap();
    
    create_note(&db, CreateNoteRequest {
        title: "Note 1".to_string(),
        body: "Body 1".to_string(),
    }).await.unwrap();
    
    create_note(&db, CreateNoteRequest {
        title: "Note 2".to_string(),
        body: "Body 2".to_string(),
    }).await.unwrap();

    let filter = NoteFilter::default();
    let notes = list_notes(&db, filter).await.unwrap();
    
    assert_eq!(notes.len(), 2);
}

#[tokio::test]
async fn test_update_note() {
    let db = Database::new("sqlite::memory:").await.unwrap();
    
    let req = CreateNoteRequest {
        title: "Original Title".to_string(),
        body: "Original Body".to_string(),
    };
    let note = create_note(&db, req).await.unwrap();

    let update_req = CreateNoteRequest {
        title: "Updated Title".to_string(),
        body: "Updated Body".to_string(),
    };
    let updated_note = update_note(&db, &note.id, update_req).await.unwrap();

    assert_eq!(updated_note.id, note.id);
    assert_eq!(updated_note.title, "Updated Title");
    assert_eq!(updated_note.body, "Updated Body");
    
    let fetched_note = get_note(&db, &note.id).await.unwrap();
    assert_eq!(fetched_note.title, "Updated Title");
}

#[tokio::test]
async fn test_delete_note() {
    let db = Database::new("sqlite::memory:").await.unwrap();
    
    let req = CreateNoteRequest {
        title: "To Be Deleted".to_string(),
        body: "Will be gone soon".to_string(),
    };
    let note = create_note(&db, req).await.unwrap();

    delete_note(&db, &note.id).await.unwrap();

    let result = get_note(&db, &note.id).await;
    assert!(result.is_err());
    
    if let Err(NoteVaultError::NotFound(msg)) = result {
        assert!(msg.contains(&note.id));
    } else {
        panic!("Expected NotFound error");
    }
}

#[tokio::test]
async fn test_search_notes() {
    let db = Database::new("sqlite::memory:").await.unwrap();
    
    create_note(&db, CreateNoteRequest {
        title: "Rust Programming".to_string(),
        body: "Learning Rust is fun".to_string(),
    }).await.unwrap();
    
    create_note(&db, CreateNoteRequest {
        title: "Grocery List".to_string(),
        body: "Apples, Bananas, Rust remover".to_string(),
    }).await.unwrap();
    
    create_note(&db, CreateNoteRequest {
        title: "Workout Plan".to_string(),
        body: "Pushups and situps".to_string(),
    }).await.unwrap();

    let filter_rust = NoteFilter {
        search: Some("Rust".to_string()),
    };
    let rust_results = search_notes(&db, &filter_rust).await.unwrap();
    assert_eq!(rust_results.len(), 2);

    let filter_apples = NoteFilter {
        search: Some("Apples".to_string()),
    };
    let apple_results = search_notes(&db, &filter_apples).await.unwrap();
    assert_eq!(apple_results.len(), 1);
    assert_eq!(apple_results[0].title, "Grocery List");
    
    let filter_empty = NoteFilter {
        search: None,
    };
    let all_results = search_notes(&db, &filter_empty).await.unwrap();
    assert_eq!(all_results.len(), 3);
}