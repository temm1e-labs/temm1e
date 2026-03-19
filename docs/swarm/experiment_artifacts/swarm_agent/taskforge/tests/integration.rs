#[cfg(test)]
mod tests {
    use taskforge::*;
    use uuid::Uuid;

    #[tokio::test]
    async fn test_create_and_get() {
        let db = Database::new("sqlite::memory:").await.unwrap();
        let req = CreateTaskRequest {
            title: "Test".into(),
            description: None,
            priority: Priority::Medium,
        };
        let task = create_task(&db, &req).await.unwrap();
        let fetched = get_task(&db, &task.id).await.unwrap();
        assert_eq!(fetched.title, "Test");
    }

    #[tokio::test]
    async fn test_list_tasks() {
        let db = Database::new("sqlite::memory:").await.unwrap();
        let req = CreateTaskRequest {
            title: "Test".into(),
            description: None,
            priority: Priority::Medium,
        };
        for _ in 0..3 {
            create_task(&db, &req).await.unwrap();
        }
        let tasks = list_tasks(&db).await.unwrap();
        assert!(tasks.len() >= 3);
    }

    #[tokio::test]
    async fn test_update_status() {
        let db = Database::new("sqlite::memory:").await.unwrap();
        let req = CreateTaskRequest {
            title: "Test".into(),
            description: None,
            priority: Priority::Medium,
        };
        let task = create_task(&db, &req).await.unwrap();
        let updated = update_status(&db, &task.id, TaskStatus::InProgress).await.unwrap();
        assert_eq!(updated.status, "inprogress");
    }

    #[tokio::test]
    async fn test_delete_task() {
        let db = Database::new("sqlite::memory:").await.unwrap();
        let req = CreateTaskRequest {
            title: "Test".into(),
            description: None,
            priority: Priority::Medium,
        };
        let task = create_task(&db, &req).await.unwrap();
        delete_task(&db, &task.id).await.unwrap();
        let result = get_task(&db, &task.id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_search_by_status() {
        let db = Database::new("sqlite::memory:").await.unwrap();
        let req = CreateTaskRequest {
            title: "Test".into(),
            description: None,
            priority: Priority::Medium,
        };
        let task1 = create_task(&db, &req).await.unwrap();
        create_task(&db, &req).await.unwrap();
        update_status(&db, &task1.id, TaskStatus::Done).await.unwrap();
        
        let filter = TaskFilter {
            status: Some(TaskStatus::Done),
            ..Default::default()
        };
        let tasks = search_tasks(&db, &filter).await.unwrap();
        assert_eq!(tasks.len(), 1);
    }
}