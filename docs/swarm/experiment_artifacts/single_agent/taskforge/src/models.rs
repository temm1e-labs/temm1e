use serde::{Serialize, Deserialize};
use sqlx::FromRow;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Priority {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum TaskStatus {
    Todo,
    InProgress,
    Done,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, FromRow)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub priority: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CreateTaskRequest {
    pub title: String,
    pub description: Option<String>,
    pub priority: Priority,
}

#[derive(Default, Clone, Debug)]
pub struct TaskFilter {
    pub status: Option<TaskStatus>,
    pub priority: Option<Priority>,
    pub search: Option<String>,
}