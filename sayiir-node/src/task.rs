//! Task metadata types exposed to Node.js.

use napi_derive::napi;
use std::time::Duration;

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
