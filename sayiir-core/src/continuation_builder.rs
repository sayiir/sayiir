//! Shared continuation builder for binding crates (Node.js, Python).
//!
//! Provides a binding-agnostic [`BuilderTask`] enum and [`build_continuation`]
//! function that both `sayiir-node` and `sayiir-py` delegate to, eliminating
//! ~135 lines of duplicated logic per binding.

use std::collections::HashMap;
use std::sync::Arc;

use crate::task::TaskMetadata;
use crate::workflow::WorkflowContinuation;

/// Validate that a duration value is finite and non-negative.
fn validate_duration(secs: f64, label: &str) -> Result<(), String> {
    if !secs.is_finite() || secs < 0.0 {
        return Err(format!("{label} must be a finite non-negative number"));
    }
    Ok(())
}

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
    /// A loop node whose body repeats until the task returns `LoopResult::Done`.
    Loop {
        /// Unique loop node identifier.
        loop_id: String,
        /// The body task identifier.
        body_task_id: String,
        /// Configuration for the body task.
        body_metadata: TaskMetadata,
        /// Maximum number of iterations before applying `on_max`.
        max_iterations: u32,
        /// What to do when `max_iterations` is reached.
        on_max: crate::workflow::MaxIterationsPolicy,
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
            version: metadata.version.clone(),
            next: current.map(Box::new),
        });
    }
    current
        .map(Box::new)
        .ok_or_else(|| "Each branch must have at least one task".to_string())
}

/// High-level builder that both binding crates (`sayiir-node`, `sayiir-py`)
/// delegate to. Manages auto-incrementing counters for lambda / loop / branch
/// IDs and collects [`BuilderTask`]s.
pub struct FlowBuilder {
    tasks: Vec<BuilderTask>,
    lambda_counter: usize,
    loop_counter: usize,
    branch_counter: usize,
}

impl FlowBuilder {
    /// Create an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self {
            tasks: Vec::new(),
            lambda_counter: 0,
            loop_counter: 0,
            branch_counter: 0,
        }
    }

    /// Generate the next lambda ID (`lambda_0`, `lambda_1`, …).
    pub fn next_lambda_id(&mut self) -> String {
        let id = format!("lambda_{}", self.lambda_counter);
        self.lambda_counter += 1;
        id
    }

    /// Add a sequential task.
    pub fn add_sequential(&mut self, task_id: String, metadata: TaskMetadata) {
        self.tasks
            .push(BuilderTask::Sequential { task_id, metadata });
    }

    /// Add a fork. Validates non-empty branches.
    ///
    /// # Errors
    ///
    /// Returns an error if branches is empty or any branch has no tasks.
    pub fn add_fork(
        &mut self,
        branches: Vec<Vec<(String, TaskMetadata)>>,
        join_id: String,
        join_metadata: TaskMetadata,
    ) -> Result<(), String> {
        if branches.is_empty() {
            return Err("Fork must have at least one branch".to_string());
        }
        for (i, branch) in branches.iter().enumerate() {
            if branch.is_empty() {
                return Err(format!("Branch {i} must have at least one task"));
            }
        }
        self.tasks.push(BuilderTask::Fork {
            branches,
            join_id,
            join_metadata,
        });
        Ok(())
    }

    /// Add a durable delay.
    ///
    /// # Errors
    ///
    /// Returns an error if `duration_secs` is negative or non-finite.
    pub fn add_delay(&mut self, delay_id: String, duration_secs: f64) -> Result<(), String> {
        validate_duration(duration_secs, "delay duration")?;
        self.tasks.push(BuilderTask::Delay {
            delay_id,
            duration_secs,
        });
        Ok(())
    }

    /// Add a signal wait.
    ///
    /// # Errors
    ///
    /// Returns an error if `timeout_secs` is negative or non-finite.
    pub fn add_signal(
        &mut self,
        signal_id: String,
        signal_name: String,
        timeout_secs: Option<f64>,
    ) -> Result<(), String> {
        if let Some(t) = timeout_secs {
            validate_duration(t, "timeout")?;
        }
        self.tasks.push(BuilderTask::AwaitSignal {
            signal_id,
            signal_name,
            timeout_secs,
        });
        Ok(())
    }

    /// Add a loop. Returns the generated loop ID.
    ///
    /// # Errors
    ///
    /// Returns an error if `max_iterations` is 0.
    pub fn add_loop(
        &mut self,
        body_task_id: String,
        body_metadata: TaskMetadata,
        max_iterations: u32,
        on_max: crate::workflow::MaxIterationsPolicy,
    ) -> Result<String, String> {
        if max_iterations == 0 {
            return Err("max_iterations must be at least 1".to_string());
        }
        let loop_id = format!("loop_{}", self.loop_counter);
        self.loop_counter += 1;
        self.tasks.push(BuilderTask::Loop {
            loop_id: loop_id.clone(),
            body_task_id,
            body_metadata,
            max_iterations,
            on_max,
        });
        Ok(loop_id)
    }

    /// Add a branch. Returns the generated branch ID.
    ///
    /// # Errors
    ///
    /// Returns an error if branches is empty or any branch has no tasks.
    pub fn add_branch(
        &mut self,
        branches: Vec<(String, Vec<(String, TaskMetadata)>)>,
        default: Option<Vec<(String, TaskMetadata)>>,
    ) -> Result<String, String> {
        if branches.is_empty() {
            return Err("route must have at least one branch".to_string());
        }
        for (key, chain) in &branches {
            if chain.is_empty() {
                return Err(format!("Branch '{key}' must have at least one task"));
            }
        }
        let branch_id = format!("branch_{}", self.branch_counter);
        self.branch_counter += 1;
        self.tasks.push(BuilderTask::Branch {
            branch_id: branch_id.clone(),
            branches,
            default,
        });
        Ok(branch_id)
    }

    /// Build the final [`WorkflowContinuation`].
    ///
    /// # Errors
    ///
    /// Returns an error if the task list is empty or any branch chain is empty.
    pub fn build(&self) -> Result<WorkflowContinuation, String> {
        build_continuation(&self.tasks)
    }
}

impl Default for FlowBuilder {
    fn default() -> Self {
        Self::new()
    }
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
#[allow(clippy::too_many_lines)]
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
                version: metadata.version.clone(),
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
                    version: join_metadata.version.clone(),
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
            BuilderTask::Loop {
                loop_id,
                body_task_id,
                body_metadata,
                max_iterations,
                on_max,
            } => {
                if *max_iterations == 0 {
                    return Err(format!(
                        "Loop '{loop_id}': max_iterations must be at least 1"
                    ));
                }
                let body = WorkflowContinuation::Task {
                    id: body_task_id.clone(),
                    func: None,
                    timeout: body_metadata.timeout,
                    retry_policy: body_metadata.retries.clone(),
                    version: body_metadata.version.clone(),
                    next: None,
                };
                WorkflowContinuation::Loop {
                    id: loop_id.clone(),
                    body: Box::new(body),
                    max_iterations: *max_iterations,
                    on_max: *on_max,
                    next: current.map(Box::new),
                }
            }
        });
    }

    current.ok_or_else(|| "Failed to build workflow".to_string())
}
