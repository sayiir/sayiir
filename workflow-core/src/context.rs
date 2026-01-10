use crate::primitives::TaskId;
use chrono::{DateTime, Utc};

/// Context to pass to a task when it is run.
///
pub struct TaskContext {
    pub task_id: TaskId,
    pub created_at: DateTime<Utc>,
}
