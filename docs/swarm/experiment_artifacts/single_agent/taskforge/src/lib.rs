pub mod error;
pub mod models;
pub mod db;
pub mod crud;
pub mod search;

pub use error::TaskForgeError;
pub use models::{Task, CreateTaskRequest, TaskFilter, Priority, TaskStatus};
pub use db::Database;
pub use crud::{create_task, get_task, list_tasks, update_status, delete_task};
pub use search::search_tasks;