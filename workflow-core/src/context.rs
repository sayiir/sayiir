use chrono::{DateTime, Utc};

/// A context to pass to a task when it is run.
///
#[derive(Debug, Clone)]
pub struct TaskContext {
    pub task_id: u64,
    pub created_at: DateTime<Utc>,
}
