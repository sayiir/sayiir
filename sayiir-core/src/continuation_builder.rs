//! Shared continuation builder for binding crates (Node.js, Python).
//!
//! Provides a binding-agnostic [`BuilderTask`] enum and [`build_continuation`]
//! function that both `sayiir-node` and `sayiir-py` delegate to, eliminating
//! ~135 lines of duplicated logic per binding.

use std::collections::HashMap;
use std::sync::Arc;

use crate::task::TaskMetadata;
use crate::workflow::WorkflowContinuation;

/// A binding-agnostic builder task.
///
/// Binding crates collect these from their language-specific APIs, then pass
/// them to [`build_continuation`] to produce the [`WorkflowContinuation`] tree.
pub enum BuilderTask {
    /// A single sequential task.
    Sequential {
        /// Unique task identifier.
        task_id: String,
        /// Task configuration (timeout, retries, display name, etc.).
        metadata: TaskMetadata,
    },
    /// A fork with parallel branches and a join task.
    Fork {
        /// Each branch is a chain of `(task_id, metadata)` pairs.
        branches: Vec<Vec<(String, TaskMetadata)>>,
        /// Identifier of the join task that combines branch results.
        join_id: String,
        /// Configuration for the join task.
        join_metadata: TaskMetadata,
    },
    /// A durable delay.
    Delay {
        /// Unique delay node identifier.
        delay_id: String,
        /// Duration in seconds.
        duration_secs: f64,
    },
    /// Wait for an external signal.
    AwaitSignal {
        /// Unique signal node identifier.
        signal_id: String,
        /// Name of the signal to wait for.
        signal_name: String,
        /// Optional timeout in seconds; `None` waits indefinitely.
        timeout_secs: Option<f64>,
    },
    /// A conditional branch node.
    Branch {
        /// Unique branch node identifier.
        branch_id: String,
        /// Each named branch is a chain of `(task_id, metadata)` pairs.
        branches: Vec<(String, Vec<(String, TaskMetadata)>)>,
        /// Optional default branch if no routing key matches.
        default: Option<Vec<(String, TaskMetadata)>>,
    },
}

/// Build a task chain from a slice of `(id, metadata)` pairs.
///
/// Returns the chain head boxed, or an error if the chain is empty.
fn build_chain(chain: &[(String, TaskMetadata)]) -> Result<Box<WorkflowContinuation>, String> {
    let mut current: Option<WorkflowContinuation> = None;
    for (id, metadata) in chain.iter().rev() {
        current = Some(WorkflowContinuation::Task {
            id: id.clone(),
            func: None,
            timeout: metadata.timeout,
            retry_policy: metadata.retries.clone(),
            next: current.map(Box::new),
        });
    }
    current
        .map(Box::new)
        .ok_or_else(|| "Each branch must have at least one task".to_string())
}

/// Build a [`WorkflowContinuation`] from a list of builder tasks.
///
/// Returns string errors — binding crates map these to their native error
/// types (e.g. `napi::Error`, `PyErr`).
///
/// # Errors
///
/// Returns an error if:
/// - `tasks` is empty
/// - Any fork or branch chain is empty
pub fn build_continuation(tasks: &[BuilderTask]) -> Result<WorkflowContinuation, String> {
    if tasks.is_empty() {
        return Err("Workflow must have at least one task".to_string());
    }

    let mut current: Option<WorkflowContinuation> = None;

    for task in tasks.iter().rev() {
        current = Some(match task {
            BuilderTask::Sequential { task_id, metadata } => WorkflowContinuation::Task {
                id: task_id.clone(),
                func: None,
                timeout: metadata.timeout,
                retry_policy: metadata.retries.clone(),
                next: current.map(Box::new),
            },
            BuilderTask::Delay {
                delay_id,
                duration_secs,
            } => WorkflowContinuation::Delay {
                id: delay_id.clone(),
                duration: std::time::Duration::from_secs_f64(*duration_secs),
                next: current.map(Box::new),
            },
            BuilderTask::AwaitSignal {
                signal_id,
                signal_name,
                timeout_secs,
            } => WorkflowContinuation::AwaitSignal {
                id: signal_id.clone(),
                signal_name: signal_name.clone(),
                timeout: timeout_secs.map(std::time::Duration::from_secs_f64),
                next: current.map(Box::new),
            },
            BuilderTask::Fork {
                branches,
                join_id,
                join_metadata,
            } => {
                let branch_ids: Vec<&str> = branches
                    .iter()
                    .filter_map(|chain| chain.first().map(|(id, _)| id.as_str()))
                    .collect();
                let fork_id = WorkflowContinuation::derive_fork_id(&branch_ids);

                let branch_conts: Vec<Arc<WorkflowContinuation>> = branches
                    .iter()
                    .map(|chain| {
                        let cont = build_chain(chain)?;
                        Ok(Arc::new(*cont))
                    })
                    .collect::<Result<Vec<_>, String>>()?;

                let join_cont = WorkflowContinuation::Task {
                    id: join_id.clone(),
                    func: None,
                    timeout: join_metadata.timeout,
                    retry_policy: join_metadata.retries.clone(),
                    next: current.map(Box::new),
                };

                WorkflowContinuation::Fork {
                    id: fork_id,
                    branches: branch_conts.into_boxed_slice(),
                    join: Some(Box::new(join_cont)),
                }
            }
            BuilderTask::Branch {
                branch_id,
                branches,
                default,
            } => {
                let branch_map: HashMap<String, Box<WorkflowContinuation>> = branches
                    .iter()
                    .map(|(key, chain)| Ok((key.clone(), build_chain(chain)?)))
                    .collect::<Result<_, String>>()?;

                let default_cont = default
                    .as_ref()
                    .map(|chain| build_chain(chain))
                    .transpose()?;

                WorkflowContinuation::Branch {
                    id: branch_id.clone(),
                    key_fn: None,
                    branches: branch_map,
                    default: default_cont,
                    next: current.map(Box::new),
                }
            }
        });
    }

    current.ok_or_else(|| "Failed to build workflow".to_string())
}
