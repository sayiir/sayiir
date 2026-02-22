//! Task metadata types exposed to Node.js.

use napi_derive::napi;
use std::time::Duration;

use sayiir_core::context::TaskExecutionContext;
use sayiir_core::task::{RetryPolicy, TaskMetadata};

/// Retry policy for task execution.
#[napi(object)]
#[derive(Clone, Default)]
pub struct NapiRetryPolicy {
    pub max_retries: u32,
    pub initial_delay_secs: f64,
    pub backoff_multiplier: f64,
    pub max_delay_secs: Option<f64>,
}

impl From<NapiRetryPolicy> for RetryPolicy {
    #[allow(clippy::cast_possible_truncation)]
    fn from(n: NapiRetryPolicy) -> Self {
        RetryPolicy {
            max_retries: n.max_retries,
            initial_delay: Duration::from_secs_f64(n.initial_delay_secs),
            backoff_multiplier: n.backoff_multiplier as f32,
            max_delay: n.max_delay_secs.map(Duration::from_secs_f64),
        }
    }
}

/// Task metadata for workflow steps.
#[napi(object)]
#[derive(Clone, Default)]
pub struct NapiTaskMetadata {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub timeout_secs: Option<f64>,
    pub retries: Option<NapiRetryPolicy>,
    pub tags: Option<Vec<String>>,
    pub version: Option<String>,
}

impl From<NapiTaskMetadata> for TaskMetadata {
    fn from(n: NapiTaskMetadata) -> Self {
        TaskMetadata {
            display_name: n.display_name,
            description: n.description,
            timeout: n.timeout_secs.map(Duration::from_secs_f64),
            retries: n.retries.map(Into::into),
            tags: n.tags.unwrap_or_default(),
            version: n.version,
        }
    }
}

/// Task execution context available from within a running task.
///
/// Provides read-only access to workflow and task metadata.
/// Retrieve via `getTaskContext()`.
#[napi(object)]
#[derive(Clone)]
pub struct NapiTaskExecutionContext {
    pub workflow_id: String,
    pub instance_id: String,
    pub task_id: String,
    pub metadata: NapiTaskMetadata,
    pub workflow_metadata: Option<serde_json::Value>,
}

impl From<TaskExecutionContext> for NapiTaskExecutionContext {
    fn from(ctx: TaskExecutionContext) -> Self {
        let workflow_metadata = ctx
            .workflow_metadata_json
            .and_then(|json| serde_json::from_str(&json).ok());
        Self {
            workflow_id: ctx.workflow_id.to_string(),
            instance_id: ctx.instance_id.to_string(),
            task_id: ctx.task_id.to_string(),
            metadata: NapiTaskMetadata {
                display_name: ctx.metadata.display_name,
                description: ctx.metadata.description,
                timeout_secs: ctx.metadata.timeout.map(|d| d.as_secs_f64()),
                retries: ctx.metadata.retries.map(|r| NapiRetryPolicy {
                    max_retries: r.max_retries,
                    initial_delay_secs: r.initial_delay.as_secs_f64(),
                    backoff_multiplier: f64::from(r.backoff_multiplier),
                    max_delay_secs: r.max_delay.map(|d| d.as_secs_f64()),
                }),
                tags: Some(ctx.metadata.tags),
                version: ctx.metadata.version,
            },
            workflow_metadata,
        }
    }
}

/// Get the current task execution context.
///
/// Returns `null` if called outside of a task execution.
#[napi]
pub fn get_task_context() -> Option<NapiTaskExecutionContext> {
    sayiir_core::context::get_task_context().map(Into::into)
}
