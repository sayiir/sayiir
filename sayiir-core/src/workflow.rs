//! Workflow structures, continuation tree, and serializable representations.
//!
//! The continuation tree ([`WorkflowContinuation`]) is the in-memory
//! representation of a workflow's execution graph. Each node is either a
//! [`Task`](WorkflowContinuation::Task),
//! [`Fork`](WorkflowContinuation::Fork),
//! [`Delay`](WorkflowContinuation::Delay),
//! [`AwaitSignal`](WorkflowContinuation::AwaitSignal), or
//! [`Branch`](WorkflowContinuation::Branch).
//!
//! [`SerializableContinuation`] strips out function pointers so the tree
//! can be persisted and later rehydrated via a [`TaskRegistry`].

use crate::context::WorkflowContext;
use crate::task::{RetryPolicy, UntypedCoreTask};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::Arc;

/// Generate a `find_duplicate_id` method for continuation-like enums
///
macro_rules! impl_find_duplicate_id {
    ($name:ident, task_fields: { $($task_extra:tt)* }, delay_extra: { $($delay_extra:tt)* }, deref_branch: $deref:expr, deref_branch_map: $deref_map:expr) => {
        impl $name {
            pub(crate) fn find_duplicate_id(&self) -> Option<String> {
                fn collect(cont: &$name, seen: &mut HashSet<String>) -> Option<String> {
                    match cont {
                        $name::Task { id, next, $($task_extra)* } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            next.as_ref().and_then(|n| collect(n, seen))
                        }
                        $name::Fork { id, branches, join } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            let deref_fn: fn(&_) -> &$name = $deref;
                            branches
                                .iter()
                                .find_map(|b| collect(deref_fn(b), seen))
                                .or_else(|| join.as_ref().and_then(|j| collect(j, seen)))
                        }
                        $name::Branch { id, branches, default, next, .. } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            let deref_map_fn: fn(&_) -> &$name = $deref_map;
                            branches
                                .values()
                                .find_map(|b| collect(deref_map_fn(b), seen))
                                .or_else(|| default.as_ref().and_then(|d| collect(d, seen)))
                                .or_else(|| next.as_ref().and_then(|n| collect(n, seen)))
                        }
                        $name::Delay { id, next, $($delay_extra)* }
                        | $name::AwaitSignal { id, next, $($delay_extra)* } => {
                            if !seen.insert(id.clone()) {
                                return Some(id.clone());
                            }
                            next.as_ref().and_then(|n| collect(n, seen))
                        }
                    }
                }
                collect(self, &mut HashSet::new())
            }
        }
    };
}

/// A workflow structure representing the tasks to execute.
pub enum WorkflowContinuation {
    /// A sequential task node.
    Task {
        /// Unique task identifier.
        id: String,
        /// Task implementation. `None` for registry-based execution
        /// where tasks are looked up by `id` at runtime.
        func: Option<UntypedCoreTask>,
        /// Maximum time the task is allowed to run before being cancelled.
        timeout: Option<std::time::Duration>,
        /// Retry policy for failed task executions.
        retry_policy: Option<RetryPolicy>,
        /// Next node in the chain.
        next: Option<Box<WorkflowContinuation>>,
    },
    /// A parallel fork node.
    Fork {
        /// Fork identifier (derived from branch IDs).
        id: String,
        /// Parallel branch continuations.
        branches: Box<[Arc<WorkflowContinuation>]>,
        /// Optional join task after all branches complete.
        join: Option<Box<WorkflowContinuation>>,
    },
    /// A durable delay node. Input passes through unchanged.
    Delay {
        /// Unique delay identifier.
        id: String,
        /// How long to wait.
        duration: std::time::Duration,
        /// Next node in the chain.
        next: Option<Box<WorkflowContinuation>>,
    },
    /// Wait for an external signal (event). Input passes through unchanged
    /// when no signal payload is provided; otherwise the signal payload
    /// becomes the input to the next step.
    AwaitSignal {
        /// Unique signal-wait identifier.
        id: String,
        /// Name of the signal to wait for.
        signal_name: String,
        /// Optional timeout duration.
        timeout: Option<std::time::Duration>,
        /// Next node in the chain.
        next: Option<Box<WorkflowContinuation>>,
    },
    /// Conditional branching node. A key function extracts a routing key
    /// from the previous step's output and dispatches to one of the named
    /// sub-continuations.
    Branch {
        /// Unique branch identifier.
        id: String,
        /// Key function implementation. `None` for registry-based execution
        /// where the key function is looked up by [`key_fn_id`] at runtime.
        key_fn: Option<UntypedCoreTask>,
        /// Named branch continuations keyed by routing key.
        branches: HashMap<String, Box<WorkflowContinuation>>,
        /// Optional default branch if no key matches.
        default: Option<Box<WorkflowContinuation>>,
        /// Continuation after the chosen branch completes.
        next: Option<Box<WorkflowContinuation>>,
    },
}

impl_find_duplicate_id!(
    WorkflowContinuation,
    task_fields: { .. },
    delay_extra: { .. },
    deref_branch: |b: &Arc<WorkflowContinuation>| -> &WorkflowContinuation { b },
    deref_branch_map: |b: &WorkflowContinuation| -> &WorkflowContinuation { b }
);

/// Derive the key-function task ID for a Branch node.
///
/// By convention the key function is registered under `"{branch_id}::key_fn"`.
/// This helper centralises that convention so callers don't repeat the suffix.
#[must_use]
pub fn key_fn_id(branch_id: &str) -> String {
    format!("{branch_id}::key_fn")
}

impl WorkflowContinuation {
    /// Derive a fork ID from a list of branch IDs.
    ///
    /// The fork ID is a concatenation of branch IDs separated by `||`.
    #[must_use]
    pub fn derive_fork_id(branch_ids: &[&str]) -> String {
        branch_ids.join("||")
    }

    /// Get the ID of this continuation node.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            WorkflowContinuation::Task { id, .. }
            | WorkflowContinuation::Fork { id, .. }
            | WorkflowContinuation::Delay { id, .. }
            | WorkflowContinuation::AwaitSignal { id, .. }
            | WorkflowContinuation::Branch { id, .. } => id,
        }
    }

    /// Get the next continuation in the chain, if any.
    ///
    #[must_use]
    pub fn get_next(&self) -> Option<&WorkflowContinuation> {
        match self {
            Self::Task { next, .. }
            | Self::Delay { next, .. }
            | Self::AwaitSignal { next, .. }
            | Self::Branch { next, .. } => next.as_deref(),
            Self::Fork { join, .. } => join.as_deref(),
        }
    }

    /// Get the first task ID from this continuation.
    ///
    /// For a `Task`, returns its ID. For a `Fork`, returns the first task ID
    /// from the first branch.
    #[must_use]
    pub fn first_task_id(&self) -> &str {
        match self {
            WorkflowContinuation::Task { id, .. }
            | WorkflowContinuation::Delay { id, .. }
            | WorkflowContinuation::AwaitSignal { id, .. }
            | WorkflowContinuation::Branch { id, .. } => id,
            WorkflowContinuation::Fork { branches, .. } => {
                if let Some(first_branch) = branches.first() {
                    first_branch.first_task_id()
                } else {
                    "unknown"
                }
            }
        }
    }

    /// Find a task node by ID (immutable).
    ///
    /// Recursively walks the full continuation tree, including through `Arc`
    /// fork branches, and returns a reference to the matching `Task` node.
    fn find_task(&self, target_id: &str) -> Option<&Self> {
        match self {
            WorkflowContinuation::Task { id, next, .. } => {
                if id == target_id {
                    return Some(self);
                }
                next.as_ref().and_then(|n| n.find_task(target_id))
            }
            WorkflowContinuation::Delay { next, .. }
            | WorkflowContinuation::AwaitSignal { next, .. } => {
                next.as_ref().and_then(|n| n.find_task(target_id))
            }
            WorkflowContinuation::Fork { branches, join, .. } => {
                for branch in branches {
                    if let Some(found) = branch.find_task(target_id) {
                        return Some(found);
                    }
                }
                join.as_ref().and_then(|j| j.find_task(target_id))
            }
            WorkflowContinuation::Branch {
                branches,
                default,
                next,
                ..
            } => {
                for branch in branches.values() {
                    if let Some(found) = branch.find_task(target_id) {
                        return Some(found);
                    }
                }
                if let Some(d) = default
                    && let Some(found) = d.find_task(target_id)
                {
                    return Some(found);
                }
                next.as_ref().and_then(|n| n.find_task(target_id))
            }
        }
    }

    /// Find a task node by ID (mutable).
    ///
    /// Same traversal as [`find_task`](Self::find_task) but returns a mutable
    /// reference. Fork branches behind `Arc` are skipped since they cannot be
    /// mutated; only the join continuation is searched.
    fn find_task_mut(&mut self, target_id: &str) -> Option<&mut Self> {
        match self {
            WorkflowContinuation::Task { id, .. } if id == target_id => Some(self),
            WorkflowContinuation::Task { next, .. } => {
                next.as_mut().and_then(|n| n.find_task_mut(target_id))
            }
            WorkflowContinuation::Delay { next, .. }
            | WorkflowContinuation::AwaitSignal { next, .. } => {
                next.as_mut().and_then(|n| n.find_task_mut(target_id))
            }
            WorkflowContinuation::Fork { join, .. } => {
                join.as_mut().and_then(|j| j.find_task_mut(target_id))
            }
            WorkflowContinuation::Branch {
                branches,
                default,
                next,
                ..
            } => {
                for branch in branches.values_mut() {
                    if let Some(found) = branch.find_task_mut(target_id) {
                        return Some(found);
                    }
                }
                if let Some(d) = default
                    && let Some(found) = d.find_task_mut(target_id)
                {
                    return Some(found);
                }
                next.as_mut().and_then(|n| n.find_task_mut(target_id))
            }
        }
    }

    /// Set the timeout on a specific task node found by ID.
    pub fn set_task_timeout(&mut self, target_id: &str, timeout: Option<std::time::Duration>) {
        if let Some(WorkflowContinuation::Task { timeout: t, .. }) = self.find_task_mut(target_id) {
            *t = timeout;
        }
    }

    /// Set the retry policy on a specific task node found by ID.
    pub fn set_task_retry_policy(&mut self, target_id: &str, policy: Option<RetryPolicy>) {
        if let Some(WorkflowContinuation::Task { retry_policy, .. }) = self.find_task_mut(target_id)
        {
            *retry_policy = policy;
        }
    }

    /// Look up the retry policy configured on a specific task by ID.
    #[must_use]
    pub fn get_task_retry_policy(&self, task_id: &str) -> Option<&RetryPolicy> {
        match self.find_task(task_id)? {
            WorkflowContinuation::Task { retry_policy, .. } => retry_policy.as_ref(),
            _ => None,
        }
    }

    /// Look up the timeout configured on a specific task by ID.
    #[must_use]
    pub fn get_task_timeout(&self, task_id: &str) -> Option<std::time::Duration> {
        match self.find_task(task_id)? {
            WorkflowContinuation::Task { timeout, .. } => *timeout,
            _ => None,
        }
    }

    /// Convert to a serializable representation (strips out task implementations).
    #[must_use]
    pub fn to_serializable(&self) -> SerializableContinuation {
        match self {
            #[allow(clippy::cast_possible_truncation)] // Durations > u64::MAX ms are not realistic
            WorkflowContinuation::Task {
                id,
                timeout,
                retry_policy,
                next,
                ..
            } => SerializableContinuation::Task {
                id: id.clone(),
                timeout_ms: timeout.map(|d| d.as_millis() as u64),
                retry_policy: retry_policy.clone(),
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
            WorkflowContinuation::Fork { id, branches, join } => SerializableContinuation::Fork {
                id: id.clone(),
                branches: branches.iter().map(|b| b.to_serializable()).collect(),
                join: join.as_ref().map(|j| Box::new(j.to_serializable())),
            },
            #[allow(clippy::cast_possible_truncation)] // Durations > u64::MAX ms are not realistic
            WorkflowContinuation::Delay { id, duration, next } => SerializableContinuation::Delay {
                id: id.clone(),
                duration_ms: duration.as_millis() as u64,
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
            #[allow(clippy::cast_possible_truncation)]
            WorkflowContinuation::AwaitSignal {
                id,
                signal_name,
                timeout,
                next,
            } => SerializableContinuation::AwaitSignal {
                id: id.clone(),
                signal_name: signal_name.clone(),
                timeout_ms: timeout.map(|d| d.as_millis() as u64),
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
            WorkflowContinuation::Branch {
                id,
                branches,
                default,
                next,
                ..
            } => SerializableContinuation::Branch {
                id: id.clone(),
                branches: branches
                    .iter()
                    .map(|(k, v)| (k.clone(), Box::new(v.to_serializable())))
                    .collect(),
                default: default.as_ref().map(|d| Box::new(d.to_serializable())),
                next: next.as_ref().map(|n| Box::new(n.to_serializable())),
            },
        }
    }

    /// Append a new node to the end of this continuation chain.
    ///
    /// Recursively walks the chain to find the tail and attaches `new_node` there.
    pub fn append_to_chain(&mut self, new_node: WorkflowContinuation) {
        match self {
            WorkflowContinuation::Task { next, .. }
            | WorkflowContinuation::Delay { next, .. }
            | WorkflowContinuation::AwaitSignal { next, .. }
            | WorkflowContinuation::Branch { next, .. } => match next {
                Some(next_box) => next_box.append_to_chain(new_node),
                None => *next = Some(Box::new(new_node)),
            },
            WorkflowContinuation::Fork { join, .. } => match join {
                Some(join_box) => join_box.append_to_chain(new_node),
                None => *join = Some(Box::new(new_node)),
            },
        }
    }
}

/// A serializable workflow continuation (stores only IDs and structure).
///
/// This type can be serialized/deserialized and later converted back into a runnable
/// `WorkflowContinuation` using a `TaskRegistry`.
///
/// # Serialization
///
/// ```rust
/// # use sayiir_core::prelude::*;
/// # use sayiir_core::codec::{Encoder, Decoder, sealed};
/// # use sayiir_core::workflow::SerializableContinuation;
/// # use bytes::Bytes;
/// # use std::sync::Arc;
/// # struct MyCodec;
/// # impl Encoder for MyCodec {}
/// # impl Decoder for MyCodec {}
/// # impl<T> sealed::EncodeValue<T> for MyCodec {
/// #     fn encode_value(&self, _: &T) -> Result<Bytes, BoxError> { Ok(Bytes::new()) }
/// # }
/// # impl<T> sealed::DecodeValue<T> for MyCodec {
/// #     fn decode_value(&self, _: Bytes) -> Result<T, BoxError> { Err("dummy".into()) }
/// # }
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let codec = Arc::new(MyCodec);
/// # let ctx = WorkflowContext::new("wf", codec.clone(), Arc::new(()));
/// # let workflow = WorkflowBuilder::new(ctx)
/// #     .with_registry()
/// #     .then("step1", |i: u32| async move { Ok(i + 1) })
/// #     .build()?;
/// # let mut registry = TaskRegistry::new();
/// # registry.register_fn("step1", codec, |i: u32| async move { Ok(i + 1) });
/// // Serialize a workflow
/// let serializable = workflow.continuation().to_serializable();
/// let json = serde_json::to_string(&serializable)?;
///
/// // Deserialize and convert to runnable
/// let serializable: SerializableContinuation = serde_json::from_str(&json)?;
/// let continuation = serializable.to_runnable(&registry)?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum SerializableContinuation {
    /// A sequential task node.
    Task {
        /// Unique task identifier.
        id: String,
        /// Optional timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Optional retry policy.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_policy: Option<RetryPolicy>,
        /// Next node in the chain.
        next: Option<Box<SerializableContinuation>>,
    },
    /// A parallel fork node.
    Fork {
        /// Fork identifier (derived from branch IDs).
        id: String,
        /// Parallel branches.
        branches: Vec<SerializableContinuation>,
        /// Optional join task after all branches complete.
        join: Option<Box<SerializableContinuation>>,
    },
    /// A durable delay node.
    Delay {
        /// Unique delay identifier.
        id: String,
        /// Duration in milliseconds.
        duration_ms: u64,
        /// Next node in the chain.
        next: Option<Box<SerializableContinuation>>,
    },
    /// A signal-wait node.
    AwaitSignal {
        /// Unique signal-wait identifier.
        id: String,
        /// Name of the signal to wait for.
        signal_name: String,
        /// Optional timeout in milliseconds.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// Next node in the chain.
        next: Option<Box<SerializableContinuation>>,
    },
    /// A conditional branching node.
    Branch {
        /// Unique branch identifier.
        id: String,
        /// Named branch continuations keyed by routing key.
        branches: HashMap<String, Box<SerializableContinuation>>,
        /// Optional default branch if no key matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<SerializableContinuation>>,
        /// Continuation after the chosen branch completes.
        next: Option<Box<SerializableContinuation>>,
    },
}

impl_find_duplicate_id!(
    SerializableContinuation,
    task_fields: { .. },
    delay_extra: { .. },
    deref_branch: |b: &SerializableContinuation| -> &SerializableContinuation { b },
    deref_branch_map: |b: &SerializableContinuation| -> &SerializableContinuation { b }
);

impl SerializableContinuation {
    /// Convert this serializable continuation into a runnable `WorkflowContinuation`.
    ///
    /// Looks up each task ID in the registry to get the actual implementation.
    ///
    /// # Errors
    ///
    /// Returns `BuildError::TaskNotFound` if any task ID is not in the registry.
    pub fn to_runnable(
        &self,
        registry: &crate::registry::TaskRegistry,
    ) -> Result<WorkflowContinuation, crate::error::BuildError> {
        if let Some(dup) = self.find_duplicate_id() {
            return Err(crate::error::BuildError::DuplicateTaskId(dup));
        }

        self.to_runnable_unchecked(registry)
    }

    /// Convert without duplicate check (called after validation).
    #[allow(clippy::too_many_lines)]
    fn to_runnable_unchecked(
        &self,
        registry: &crate::registry::TaskRegistry,
    ) -> Result<WorkflowContinuation, crate::error::BuildError> {
        match self {
            SerializableContinuation::Task {
                id,
                timeout_ms,
                retry_policy,
                next,
            } => {
                let func = registry
                    .get(id)
                    .ok_or_else(|| crate::error::BuildError::TaskNotFound(id.clone()))?;
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Task {
                    id: id.clone(),
                    func: Some(func),
                    timeout: timeout_ms.map(std::time::Duration::from_millis),
                    retry_policy: retry_policy.clone(),
                    next,
                })
            }
            SerializableContinuation::Fork { id, branches, join } => {
                let branches: Result<Vec<_>, _> = branches
                    .iter()
                    .map(|b| b.to_runnable_unchecked(registry).map(Arc::new))
                    .collect();
                let join = join
                    .as_ref()
                    .map(|j| j.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Fork {
                    id: id.clone(),
                    branches: branches?.into_boxed_slice(),
                    join,
                })
            }
            SerializableContinuation::Delay {
                id,
                duration_ms,
                next,
            } => {
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Delay {
                    id: id.clone(),
                    duration: std::time::Duration::from_millis(*duration_ms),
                    next,
                })
            }
            SerializableContinuation::AwaitSignal {
                id,
                signal_name,
                timeout_ms,
                next,
            } => {
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::AwaitSignal {
                    id: id.clone(),
                    signal_name: signal_name.clone(),
                    timeout: timeout_ms.map(std::time::Duration::from_millis),
                    next,
                })
            }
            SerializableContinuation::Branch {
                id,
                branches,
                default,
                next,
            } => {
                let kf_id = key_fn_id(id);
                let key_fn = registry
                    .get(&kf_id)
                    .ok_or(crate::error::BuildError::TaskNotFound(kf_id))?;
                let branches: Result<HashMap<_, _>, _> = branches
                    .iter()
                    .map(|(k, v)| {
                        v.to_runnable_unchecked(registry)
                            .map(|c| (k.clone(), Box::new(c)))
                    })
                    .collect();
                let default = default
                    .as_ref()
                    .map(|d| d.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                let next = next
                    .as_ref()
                    .map(|n| n.to_runnable_unchecked(registry).map(Box::new))
                    .transpose()?;
                Ok(WorkflowContinuation::Branch {
                    id: id.clone(),
                    key_fn: Some(key_fn),
                    branches: branches?,
                    default,
                    next,
                })
            }
        }
    }

    /// Get all task IDs referenced in this continuation.
    #[must_use]
    pub fn task_ids(&self) -> Vec<&str> {
        fn collect<'a>(cont: &'a SerializableContinuation, ids: &mut Vec<&'a str>) {
            match cont {
                SerializableContinuation::Task { id, next, .. }
                | SerializableContinuation::Delay { id, next, .. }
                | SerializableContinuation::AwaitSignal { id, next, .. } => {
                    ids.push(id.as_str());
                    if let Some(n) = next {
                        collect(n, ids);
                    }
                }
                SerializableContinuation::Fork { id, branches, join } => {
                    ids.push(id.as_str());
                    for b in branches {
                        collect(b, ids);
                    }
                    if let Some(j) = join {
                        collect(j, ids);
                    }
                }
                SerializableContinuation::Branch {
                    id,
                    branches,
                    default,
                    next,
                } => {
                    ids.push(id.as_str());
                    for b in branches.values() {
                        collect(b, ids);
                    }
                    if let Some(d) = default {
                        collect(d, ids);
                    }
                    if let Some(n) = next {
                        collect(n, ids);
                    }
                }
            }
        }
        let mut ids = Vec::new();
        collect(self, &mut ids);
        ids
    }

    /// Compute a SHA256 hash of this continuation's structure.
    ///
    /// This hash serves as a "version" identifier for the workflow definition.
    /// It can be used to detect when a serialized workflow state was created
    /// with a different workflow definition than the current one.
    ///
    /// The hash is computed from the canonical structure of task IDs and their
    /// arrangement.
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn compute_definition_hash(&self) -> String {
        #[allow(clippy::too_many_lines)]
        fn hash_continuation(cont: &SerializableContinuation, hasher: &mut Sha256) {
            match cont {
                SerializableContinuation::Task {
                    id,
                    timeout_ms,
                    retry_policy,
                    next,
                } => {
                    hasher.update(b"T:"); // Tag for Task
                    hasher.update(id.as_bytes());
                    if let Some(ms) = timeout_ms {
                        hasher.update(b":t:");
                        hasher.update(ms.to_string().as_bytes());
                    }
                    if let Some(rp) = retry_policy {
                        hasher.update(b":r:");
                        hasher.update(rp.max_retries.to_string().as_bytes());
                        hasher.update(b":");
                        hasher.update(rp.initial_delay.as_millis().to_string().as_bytes());
                        hasher.update(b":");
                        hasher.update(rp.backoff_multiplier.to_string().as_bytes());
                    }
                    hasher.update(b";");
                    if let Some(n) = next {
                        hash_continuation(n, hasher);
                    }
                }
                SerializableContinuation::Fork { id, branches, join } => {
                    hasher.update(b"F:");
                    hasher.update(id.as_bytes());
                    hasher.update(b"[");
                    for branch in branches {
                        hash_continuation(branch, hasher);
                        hasher.update(b",");
                    }
                    hasher.update(b"]");
                    if let Some(j) = join {
                        hasher.update(b"J:");
                        hash_continuation(j, hasher);
                    }
                }
                SerializableContinuation::Delay {
                    id,
                    duration_ms,
                    next,
                } => {
                    hasher.update(b"D:");
                    hasher.update(id.as_bytes());
                    hasher.update(b":");
                    hasher.update(duration_ms.to_string().as_bytes());
                    hasher.update(b";");
                    if let Some(n) = next {
                        hash_continuation(n, hasher);
                    }
                }
                SerializableContinuation::AwaitSignal {
                    id,
                    signal_name,
                    timeout_ms,
                    next,
                } => {
                    hasher.update(b"S:");
                    hasher.update(id.as_bytes());
                    hasher.update(b":");
                    hasher.update(signal_name.as_bytes());
                    if let Some(ms) = timeout_ms {
                        hasher.update(b":t:");
                        hasher.update(ms.to_string().as_bytes());
                    }
                    hasher.update(b";");
                    if let Some(n) = next {
                        hash_continuation(n, hasher);
                    }
                }
                SerializableContinuation::Branch {
                    id,
                    branches,
                    default,
                    next,
                } => {
                    hasher.update(b"B:");
                    hasher.update(id.as_bytes());
                    hasher.update(b"{");
                    // Sort keys for deterministic hashing
                    let mut keys: Vec<&String> = branches.keys().collect();
                    keys.sort();
                    for key in keys {
                        hasher.update(key.as_bytes());
                        hasher.update(b"=>");
                        if let Some(branch) = branches.get(key) {
                            hash_continuation(branch, hasher);
                        }
                        hasher.update(b",");
                    }
                    hasher.update(b"}");
                    if let Some(d) = default {
                        hasher.update(b"_=>");
                        hash_continuation(d, hasher);
                    }
                    hasher.update(b";");
                    if let Some(n) = next {
                        hash_continuation(n, hasher);
                    }
                }
            }
        }

        let mut hasher = Sha256::new();
        hash_continuation(self, &mut hasher);
        let result = hasher.finalize();
        format!("{result:x}")
    }
}

/// A complete serializable workflow state including version information.
///
/// This type wraps `SerializableContinuation` with workflow identification and
/// a definition hash that serves as a version check. When deserializing, the
/// hash is verified to ensure the serialized state matches the current workflow
/// definition.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SerializedWorkflowState {
    /// The workflow identifier.
    pub workflow_id: String,
    /// SHA256 hash of the workflow definition structure.
    /// Used to detect version mismatches during deserialization.
    pub definition_hash: String,
    /// The serializable continuation structure.
    pub continuation: SerializableContinuation,
}

/// The status of a workflow execution.
#[derive(Debug)]
pub enum WorkflowStatus {
    /// The workflow is still in progress (task completed, workflow continues).
    InProgress,
    /// The workflow completed successfully.
    Completed,
    /// The workflow failed with an error.
    Failed(String),
    /// The workflow was cancelled.
    Cancelled {
        /// Optional reason for the cancellation.
        reason: Option<String>,
        /// Optional identifier of who cancelled the workflow.
        cancelled_by: Option<String>,
    },
    /// The workflow was paused.
    Paused {
        /// Optional reason for the pause.
        reason: Option<String>,
        /// Optional identifier of who paused the workflow.
        paused_by: Option<String>,
    },
    /// The workflow is waiting for a delay to expire.
    Waiting {
        /// When the delay expires.
        wake_at: chrono::DateTime<chrono::Utc>,
        /// The delay node ID.
        delay_id: String,
    },
    /// The workflow is waiting for an external signal.
    AwaitingSignal {
        /// The signal node ID.
        signal_id: String,
        /// The named signal being waited on.
        signal_name: String,
        /// Optional timeout deadline.
        wake_at: Option<chrono::DateTime<chrono::Utc>>,
    },
}

// Re-export builder types for backwards compatibility.
pub use crate::builder::{
    BranchCollector, ContinuationState, ForkBuilder, NoContinuation, NoRegistry, RegistryBehavior,
    RouteBuilder, SubBuilder, WorkflowBuilder,
};

use crate::registry::TaskRegistry;

/// A built workflow that can be executed.
pub struct Workflow<C, Input, M = ()> {
    pub(crate) definition_hash: String,
    pub(crate) context: WorkflowContext<C, M>,
    pub(crate) continuation: WorkflowContinuation,
    pub(crate) _phantom: PhantomData<Input>,
}

impl<C, Input, M> Workflow<C, Input, M> {
    /// Get the workflow ID.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        &self.context.workflow_id
    }

    /// Get the definition hash.
    ///
    /// This hash is computed from the workflow's continuation structure and serves
    /// as a version identifier. It can be used to detect when a serialized workflow
    /// state was created with a different workflow definition.
    #[must_use]
    pub fn definition_hash(&self) -> &str {
        &self.definition_hash
    }

    /// Get a reference to the context of this workflow.
    #[must_use]
    pub fn context(&self) -> &WorkflowContext<C, M> {
        &self.context
    }

    /// Get a reference to the codec used by this workflow.
    #[must_use]
    pub fn codec(&self) -> &Arc<C> {
        &self.context.codec
    }

    /// Get a reference to the continuation of this workflow.
    #[must_use]
    pub fn continuation(&self) -> &WorkflowContinuation {
        &self.continuation
    }

    /// Get a reference to the metadata attached to this workflow.
    #[must_use]
    pub fn metadata(&self) -> &Arc<M> {
        &self.context.metadata
    }
}

// ============================================================================
// Serializable Workflow
// ============================================================================

/// A workflow that can be serialized and deserialized.
///
/// This is a wrapper around `Workflow` that carries an internal `TaskRegistry`,
/// automatically populated during building. This enables serialization without
/// manually setting up a separate registry.
///
/// # Example
///
/// ```rust
/// # use sayiir_core::prelude::*;
/// # use sayiir_core::codec::{Encoder, Decoder, sealed};
/// # use sayiir_core::workflow::SerializedWorkflowState;
/// # use bytes::Bytes;
/// # use std::sync::Arc;
/// # struct MyCodec;
/// # impl Encoder for MyCodec {}
/// # impl Decoder for MyCodec {}
/// # impl<T> sealed::EncodeValue<T> for MyCodec {
/// #     fn encode_value(&self, _: &T) -> Result<Bytes, BoxError> { Ok(Bytes::new()) }
/// # }
/// # impl<T> sealed::DecodeValue<T> for MyCodec {
/// #     fn decode_value(&self, _: Bytes) -> Result<T, BoxError> { Err("dummy".into()) }
/// # }
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// # let codec = Arc::new(MyCodec);
/// # let ctx = WorkflowContext::new("my-workflow", codec, Arc::new(()));
/// // Build a serializable workflow
/// let workflow = WorkflowBuilder::new(ctx)
///     .with_registry()  // Enable serialization
///     .then("step1", |i: u32| async move { Ok(i + 1) })
///     .then("step2", |i: u32| async move { Ok(i * 2) })
///     .build()?;
///
/// // Serialize
/// let serialized = workflow.to_serializable();
/// let json = serde_json::to_string(&serialized)?;
///
/// // Deserialize (uses internal registry)
/// let deserialized: SerializedWorkflowState = serde_json::from_str(&json)?;
/// let restored = workflow.to_runnable(&deserialized)?;
/// # Ok(())
/// # }
/// ```
pub struct SerializableWorkflow<C, Input, M = ()> {
    pub(crate) inner: Workflow<C, Input, M>,
    pub(crate) registry: TaskRegistry,
}

impl<C, Input, M> SerializableWorkflow<C, Input, M> {
    /// Get the workflow ID.
    #[must_use]
    pub fn workflow_id(&self) -> &str {
        self.inner.workflow_id()
    }

    /// Get the definition hash.
    #[must_use]
    pub fn definition_hash(&self) -> &str {
        self.inner.definition_hash()
    }

    /// Get a reference to the inner workflow.
    #[must_use]
    pub fn workflow(&self) -> &Workflow<C, Input, M> {
        &self.inner
    }

    /// Get a reference to the context.
    #[must_use]
    pub fn context(&self) -> &WorkflowContext<C, M> {
        self.inner.context()
    }

    /// Get a reference to the codec.
    #[must_use]
    pub fn codec(&self) -> &Arc<C> {
        self.inner.codec()
    }

    /// Get a reference to the continuation.
    #[must_use]
    pub fn continuation(&self) -> &WorkflowContinuation {
        self.inner.continuation()
    }

    /// Get a reference to the metadata.
    #[must_use]
    pub fn metadata(&self) -> &Arc<M> {
        self.inner.metadata()
    }

    /// Get a reference to the internal task registry.
    #[must_use]
    pub fn registry(&self) -> &TaskRegistry {
        &self.registry
    }

    /// Convert to a serializable state representation.
    ///
    /// Returns a `SerializedWorkflowState` that includes the workflow ID,
    /// definition hash, and continuation structure. This can be serialized
    /// and later deserialized to resume the workflow.
    #[must_use]
    pub fn to_serializable(&self) -> SerializedWorkflowState {
        SerializedWorkflowState {
            workflow_id: self.inner.workflow_id().to_string(),
            definition_hash: self.inner.definition_hash.clone(),
            continuation: self.inner.continuation().to_serializable(),
        }
    }

    /// Convert a serialized workflow state to runnable using the internal registry.
    ///
    /// # Errors
    ///
    /// Returns `BuildError::DefinitionMismatch` if the definition hash doesn't
    /// match this workflow's hash, indicating the serialized state was created with
    /// a different workflow definition.
    ///
    /// Returns `BuildError::TaskNotFound` if any task ID is not in the registry.
    pub fn to_runnable(
        &self,
        state: &SerializedWorkflowState,
    ) -> Result<WorkflowContinuation, crate::error::BuildError> {
        if state.definition_hash != self.inner.definition_hash {
            return Err(crate::error::BuildError::DefinitionMismatch {
                expected: self.inner.definition_hash.clone(),
                found: state.definition_hash.clone(),
            });
        }
        state.continuation.to_runnable(&self.registry)
    }
}

impl<C, Input, M> Deref for SerializableWorkflow<C, Input, M> {
    type Target = Workflow<C, Input, M>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use crate::codec::{Decoder, Encoder, sealed};
    use crate::error::BoxError;
    use crate::workflow::WorkflowBuilder;
    use bytes::Bytes;

    struct DummyCodec;

    impl Encoder for DummyCodec {}
    impl Decoder for DummyCodec {}

    impl<Input> sealed::EncodeValue<Input> for DummyCodec {
        fn encode_value(&self, _value: &Input) -> Result<Bytes, BoxError> {
            Ok(Bytes::new())
        }
    }
    impl<Output> sealed::DecodeValue<Output> for DummyCodec {
        fn decode_value(&self, _bytes: Bytes) -> Result<Output, BoxError> {
            Err("Not implemented".into())
        }
    }

    #[test]
    fn test_workflow_build() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Verify the workflow was built successfully
        // The workflow can be executed using a WorkflowRunner from sayiir-runtime
        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_workflow_with_metadata() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new(
            "test-workflow",
            Arc::new(DummyCodec),
            Arc::new("test_metadata"),
        );
        let workflow: Workflow<DummyCodec, u32, &str> = WorkflowBuilder::new(ctx)
            .then("test", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        assert_eq!(**workflow.metadata(), "test_metadata");
    }

    #[test]
    fn test_task_order() {
        use crate::context::WorkflowContext;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("first", |i: u32| async move { Ok(i + 1) })
            .then("second", |i: u32| async move { Ok(i + 2) })
            .then("third", |i: u32| async move { Ok(i + 3) })
            .build()
            .unwrap();

        // Verify the continuation chain structure
        // Tasks should be linked in order: first -> second -> third
        let mut current = workflow.continuation();
        let mut task_ids = Vec::new();

        loop {
            match current {
                crate::workflow::WorkflowContinuation::Task { id, next, .. } => {
                    task_ids.push(id.clone());
                    match next {
                        Some(next_box) => current = next_box.as_ref(),
                        None => break,
                    }
                }
                _ => break,
            }
        }

        assert_eq!(
            task_ids,
            vec!["first", "second", "third"],
            "Tasks should execute in the order they were added"
        );
    }

    #[test]
    fn test_heterogeneous_fork_join_compiles() {
        use crate::context::WorkflowContext;
        use crate::task::BranchOutputs;
        use crate::workflow::Workflow;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        // This test verifies that the heterogeneous fork-join API compiles correctly.
        // Each branch can return a different type thanks to type erasure.
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                // Returns u32
                b.add("count", |i: u32| async move { Ok(i * 2) });
                // Returns String - heterogeneous output type!
                b.add("name", |i: u32| async move { Ok(format!("item_{}", i)) });
                // Returns f64 - another different type!
                b.add("ratio", |i: u32| async move { Ok(i as f64 / 100.0) });
            })
            .join("combine", |outputs: BranchOutputs<DummyCodec>| async move {
                // In a real workflow with a proper codec, you would:
                // let count: u32 = outputs.get("count")?;
                // let name: String = outputs.get("name")?;
                // let ratio: f64 = outputs.get("ratio")?;
                // For this test, just verify the API compiles
                let _ = outputs.len();
                Ok(format!("combined {} branches", outputs.len()))
            })
            .then("final", |s: String| async move { Ok(s.len() as u32) })
            .build()
            .unwrap();

        let _workflow_ref = &workflow;
    }

    #[test]
    fn test_duplicate_branch_id_returns_error() {
        use crate::context::WorkflowContext;
        use crate::error::BuildError;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let result = WorkflowBuilder::<_, u32, _>::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("count", |i: u32| async move { Ok(i * 2) });
                b.add("count", |i: u32| async move { Ok(i * 3) }); // Duplicate!
            })
            .join("combine", |_outputs| async move { Ok(0u32) })
            .build();

        assert!(matches!(
            result,
            Err(BuildError::DuplicateTaskId(id)) if id == "count"
        ));
    }

    #[test]
    fn test_serializable_continuation() {
        use crate::context::WorkflowContext;
        use crate::error::BuildError;
        use crate::registry::TaskRegistry;
        use std::sync::Arc;

        // Build a workflow
        let codec = Arc::new(DummyCodec);
        let ctx = WorkflowContext::new("test-workflow", codec.clone(), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Convert to serializable
        let serializable = workflow.continuation().to_serializable();

        // Check structure
        let task_ids = serializable.task_ids();
        assert_eq!(task_ids, vec!["step1", "step2"]);

        // Hydration fails without registry
        let empty_registry = TaskRegistry::new();
        let result = serializable.to_runnable(&empty_registry);
        assert!(matches!(result, Err(BuildError::TaskNotFound(id)) if id == "step1"));

        // Hydration succeeds with proper registry
        let mut registry = TaskRegistry::new();
        registry.register_fn("step1", codec.clone(), |i: u32| async move { Ok(i + 1) });
        registry.register_fn("step2", codec.clone(), |i: u32| async move { Ok(i * 2) });

        let hydrated = serializable.to_runnable(&registry);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_serializable_fork_join() {
        use crate::context::WorkflowContext;
        use crate::task::BranchOutputs;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("prepare", |i: u32| async move { Ok(i) })
            .branches(|b| {
                b.add("branch_a", |i: u32| async move { Ok(i * 2) });
                b.add("branch_b", |i: u32| async move { Ok(i + 10) });
            })
            .join(
                "merge",
                |_: BranchOutputs<DummyCodec>| async move { Ok(0u32) },
            )
            .build()
            .unwrap();

        let serializable = workflow.continuation().to_serializable();
        let task_ids = serializable.task_ids();

        // Should contain: prepare, fork (branch_a||branch_b), branch_a, branch_b, merge
        assert!(task_ids.contains(&"prepare"));
        assert!(task_ids.contains(&"branch_a||branch_b"));
        assert!(task_ids.contains(&"branch_a"));
        assert!(task_ids.contains(&"branch_b"));
        assert!(task_ids.contains(&"merge"));
        assert_eq!(task_ids.len(), 5);
    }

    #[test]
    fn test_serializable_workflow_builder() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);
        let ctx = WorkflowContext::new("test-workflow", codec, Arc::new(()));

        // Build with with_registry() - registry is auto-populated
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Registry was auto-populated
        assert!(workflow.registry().contains("step1"));
        assert!(workflow.registry().contains("step2"));
        assert_eq!(workflow.registry().len(), 2);

        // Can serialize
        let serializable = workflow.to_serializable();
        assert_eq!(serializable.continuation.task_ids(), vec!["step1", "step2"]);

        // Can hydrate using internal registry
        let hydrated = workflow.to_runnable(&serializable);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_with_existing_registry_and_then_registered() {
        use crate::context::WorkflowContext;
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);

        // Pre-register tasks in a registry
        let mut registry = TaskRegistry::new();
        registry.register_fn("double", codec.clone(), |i: u32| async move { Ok(i * 2) });
        registry.register_fn("add_ten", codec.clone(), |i: u32| async move { Ok(i + 10) });

        // Build workflow using existing registry and referencing pre-registered tasks
        let ctx = WorkflowContext::new("test-workflow", codec.clone(), Arc::new(()));
        let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
            .with_existing_registry(registry)
            .then_registered::<u32>("double")
            .unwrap()
            .then_registered::<u32>("add_ten")
            .unwrap()
            .build()
            .unwrap();

        // Registry should contain the pre-registered tasks
        assert!(workflow.registry().contains("double"));
        assert!(workflow.registry().contains("add_ten"));

        // Workflow structure should reference those tasks
        let serializable = workflow.to_serializable();
        assert_eq!(
            serializable.continuation.task_ids(),
            vec!["double", "add_ten"]
        );

        // Can hydrate using the same registry
        let hydrated = workflow.to_runnable(&serializable);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_mixed_inline_and_registered_tasks() {
        use crate::context::WorkflowContext;
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);

        // Pre-register one task
        let mut registry = TaskRegistry::new();
        registry.register_fn(
            "preregistered",
            codec.clone(),
            |i: u32| async move { Ok(i * 2) },
        );

        // Build workflow mixing pre-registered and inline tasks
        let ctx = WorkflowContext::new("test-workflow", codec.clone(), Arc::new(()));
        let workflow: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx)
            .with_existing_registry(registry)
            .then_registered::<u32>("preregistered") // Use pre-registered
            .unwrap()
            .then("inline", |i: u32| async move { Ok(i + 5) }) // Define inline
            .build()
            .unwrap();

        // Registry should have both tasks
        assert!(workflow.registry().contains("preregistered"));
        assert!(workflow.registry().contains("inline"));
        assert_eq!(workflow.registry().len(), 2);
    }

    #[test]
    fn test_workflow_id_and_definition_hash() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("my-workflow-id", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Check workflow_id is set correctly
        assert_eq!(workflow.workflow_id(), "my-workflow-id");

        // Definition hash should be non-empty
        assert!(!workflow.definition_hash().is_empty());

        // Serializable state should contain the same id and hash
        let state = workflow.to_serializable();
        assert_eq!(state.workflow_id, "my-workflow-id");
        assert_eq!(state.definition_hash, workflow.definition_hash());
    }

    #[test]
    fn test_definition_hash_changes_with_structure() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        // Build two workflows with different structures
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow1 = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow2 = WorkflowBuilder::new(ctx2)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        assert_ne!(workflow1.definition_hash(), workflow2.definition_hash());
    }

    #[test]
    fn test_definition_mismatch_error() {
        use crate::context::WorkflowContext;
        use crate::error::BuildError;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Create a state with wrong hash
        let mut state = workflow.to_serializable();
        state.definition_hash = "wrong-hash".to_string();

        // to_runnable should fail with DefinitionMismatch
        let result = workflow.to_runnable(&state);
        assert!(matches!(result, Err(BuildError::DefinitionMismatch { .. })));
    }

    #[test]
    fn test_duplicate_id_tampering_detection() {
        use crate::error::BuildError;
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableContinuation;
        use std::sync::Arc;

        let codec = Arc::new(DummyCodec);

        // Create a registry with tasks
        let mut registry = TaskRegistry::new();
        registry.register_fn("step1", codec.clone(), |i: u32| async move { Ok(i + 1) });
        registry.register_fn("step2", codec.clone(), |i: u32| async move { Ok(i * 2) });

        // Manually construct a tampered continuation with duplicate IDs
        let tampered = SerializableContinuation::Task {
            id: "step1".to_string(),
            timeout_ms: None,
            retry_policy: None,
            next: Some(Box::new(SerializableContinuation::Task {
                id: "step1".to_string(), // Duplicate!
                timeout_ms: None,
                retry_policy: None,
                next: None,
            })),
        };

        // to_runnable should detect the tampering
        let result = tampered.to_runnable(&registry);
        assert!(matches!(
            result,
            Err(BuildError::DuplicateTaskId(id)) if id == "step1"
        ));
    }

    // ========================================================================
    // Delay tests
    // ========================================================================

    #[test]
    fn test_delay_builder() {
        use crate::context::WorkflowContext;
        use crate::workflow::{Workflow, WorkflowContinuation};
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_1s", Duration::from_secs(1))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Verify the chain structure: Task -> Delay -> Task
        let mut ids = Vec::new();
        let mut current = workflow.continuation();
        loop {
            match current {
                WorkflowContinuation::Task { id, next, .. } => {
                    ids.push(format!("task:{id}"));
                    match next {
                        Some(n) => current = n,
                        None => break,
                    }
                }
                WorkflowContinuation::Delay {
                    id, duration, next, ..
                } => {
                    ids.push(format!("delay:{id}:{}ms", duration.as_millis()));
                    match next {
                        Some(n) => current = n,
                        None => break,
                    }
                }
                _ => break,
            }
        }

        assert_eq!(
            ids,
            vec!["task:step1", "delay:wait_1s:1000ms", "task:step2"]
        );
    }

    #[test]
    fn test_delay_serialization_roundtrip() {
        use crate::context::WorkflowContext;
        use crate::workflow::SerializableContinuation;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait_5s", Duration::from_secs(5))
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Convert to serializable
        let serializable = workflow.to_serializable();

        // Check structure
        let task_ids = serializable.continuation.task_ids();
        assert_eq!(task_ids, vec!["step1", "wait_5s", "step2"]);

        // Check delay duration is preserved
        match &serializable.continuation {
            SerializableContinuation::Task { next, .. } => {
                let next = next.as_ref().unwrap();
                match next.as_ref() {
                    SerializableContinuation::Delay {
                        id, duration_ms, ..
                    } => {
                        assert_eq!(id, "wait_5s");
                        assert_eq!(*duration_ms, 5000);
                    }
                    other => panic!("Expected Delay, got {other:?}"),
                }
            }
            other => panic!("Expected Task, got {other:?}"),
        }

        // Hydrate back to runnable
        let hydrated = workflow.to_runnable(&serializable);
        assert!(hydrated.is_ok());
    }

    #[test]
    fn test_delay_first_task_id() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .delay("initial_delay", Duration::from_secs(10))
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        assert_eq!(workflow.continuation().first_task_id(), "initial_delay");
    }

    #[test]
    fn test_delay_duplicate_id_detection() {
        use crate::context::WorkflowContext;
        use crate::error::BuildError;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let result = WorkflowBuilder::<_, u32, _>::new(ctx)
            .then("dup", |i: u32| async move { Ok(i + 1) })
            .delay("dup", Duration::from_secs(1))
            .build();

        assert!(matches!(
            result,
            Err(BuildError::DuplicateTaskId(id)) if id == "dup"
        ));
    }

    #[test]
    fn test_delay_definition_hash_includes_duration() {
        use crate::context::WorkflowContext;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;
        use std::time::Duration;

        // Workflow with 1-second delay
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf1: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait", Duration::from_secs(1))
            .build()
            .unwrap();

        // Workflow with 60-second delay (same ID, different duration)
        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf2: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx2)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .delay("wait", Duration::from_secs(60))
            .build()
            .unwrap();

        // Hashes should differ because duration differs
        assert_ne!(wf1.definition_hash(), wf2.definition_hash());
    }

    #[test]
    fn test_delay_definition_hash_differs_from_task() {
        use crate::context::WorkflowContext;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;
        use std::time::Duration;

        // Workflow with task
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf1: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Workflow with delay instead
        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf2: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx2)
            .with_registry()
            .delay("step1", Duration::from_secs(1))
            .build()
            .unwrap();

        // Hashes should differ (Task vs Delay are tagged differently)
        assert_ne!(wf1.definition_hash(), wf2.definition_hash());
    }

    #[test]
    fn test_delay_task_ids() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .then("fetch", |i: u32| async move { Ok(i) })
            .delay("wait_24h", Duration::from_secs(86400))
            .then("process", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let serializable = workflow.continuation().to_serializable();
        let ids = serializable.task_ids();
        assert_eq!(ids, vec!["fetch", "wait_24h", "process"]);
    }

    #[test]
    fn test_delay_only_workflow() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;
        use std::time::Duration;

        use crate::workflow::Workflow;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow: Workflow<DummyCodec, u32> = WorkflowBuilder::new(ctx)
            .delay("just_wait", Duration::from_millis(10))
            .build()
            .unwrap();

        assert_eq!(workflow.continuation().first_task_id(), "just_wait");

        let serializable = workflow.continuation().to_serializable();
        assert_eq!(serializable.task_ids(), vec!["just_wait"]);
    }

    #[test]
    fn test_delay_to_runnable_no_registry_needed() {
        use crate::registry::TaskRegistry;
        use crate::workflow::SerializableContinuation;

        // A delay doesn't need a registry entry (it has no func)
        let delay = SerializableContinuation::Delay {
            id: "wait".to_string(),
            duration_ms: 5000,
            next: None,
        };

        let empty_registry = TaskRegistry::new();
        let result = delay.to_runnable(&empty_registry);
        assert!(result.is_ok());

        let runnable = result.unwrap();
        match runnable {
            crate::workflow::WorkflowContinuation::Delay {
                id, duration, next, ..
            } => {
                assert_eq!(id, "wait");
                assert_eq!(duration, std::time::Duration::from_millis(5000));
                assert!(next.is_none());
            }
            _ => panic!("Expected Delay variant"),
        }
    }

    // ========================================================================
    // Timeout tests
    // ========================================================================

    #[test]
    fn test_timeout_serialization_roundtrip() {
        use crate::context::WorkflowContext;
        use crate::task::TaskMetadata;
        use crate::workflow::SerializableContinuation;
        use std::sync::Arc;
        use std::time::Duration;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .with_metadata(TaskMetadata {
                timeout: Some(Duration::from_secs(30)),
                ..Default::default()
            })
            .then("step2", |i: u32| async move { Ok(i * 2) })
            .build()
            .unwrap();

        // Convert to serializable
        let serializable = workflow.to_serializable();

        // Check timeout is preserved in serialization
        match &serializable.continuation {
            SerializableContinuation::Task { id, timeout_ms, .. } => {
                assert_eq!(id, "step1");
                assert_eq!(*timeout_ms, Some(30_000));
            }
            other => panic!("Expected Task, got {other:?}"),
        }

        // Hydrate back to runnable and verify timeout
        let hydrated = workflow.to_runnable(&serializable).unwrap();
        match &hydrated {
            crate::workflow::WorkflowContinuation::Task { id, timeout, .. } => {
                assert_eq!(id, "step1");
                assert_eq!(*timeout, Some(Duration::from_secs(30)));
            }
            _ => panic!("Expected Task variant"),
        }
    }

    #[test]
    fn test_timeout_changes_definition_hash() {
        use crate::context::WorkflowContext;
        use crate::task::TaskMetadata;
        use crate::workflow::SerializableWorkflow;
        use std::sync::Arc;
        use std::time::Duration;

        // Workflow without timeout
        let ctx1 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf1: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx1)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        // Workflow with timeout (same ID, different timeout)
        let ctx2 = WorkflowContext::new("workflow", Arc::new(DummyCodec), Arc::new(()));
        let wf2: SerializableWorkflow<_, u32> = WorkflowBuilder::new(ctx2)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .with_metadata(TaskMetadata {
                timeout: Some(Duration::from_secs(30)),
                ..Default::default()
            })
            .build()
            .unwrap();

        // Hashes should differ because timeout differs
        assert_ne!(wf1.definition_hash(), wf2.definition_hash());
    }

    #[test]
    fn test_no_timeout_field_absent_in_serialization() {
        use crate::context::WorkflowContext;
        use std::sync::Arc;

        let ctx = WorkflowContext::new("test-workflow", Arc::new(DummyCodec), Arc::new(()));
        let workflow = WorkflowBuilder::new(ctx)
            .with_registry()
            .then("step1", |i: u32| async move { Ok(i + 1) })
            .build()
            .unwrap();

        let serializable = workflow.to_serializable();
        // With serde skip_serializing_if, timeout_ms should not appear in JSON
        let json = serde_json::to_string(&serializable.continuation).unwrap();
        assert!(
            !json.contains("timeout_ms"),
            "timeout_ms should be absent when None: {json}"
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
mod proptests {
    use super::SerializableContinuation;
    use proptest::prelude::*;

    /// Strategy for alphanumeric IDs (1..8 chars).
    fn arb_id() -> impl Strategy<Value = String> {
        "[a-z0-9]{1,8}"
    }

    /// Recursive strategy for `SerializableContinuation` with bounded depth.
    fn arb_continuation(depth: usize) -> BoxedStrategy<SerializableContinuation> {
        let leaf = arb_id().prop_map(|id| SerializableContinuation::Task {
            id,
            timeout_ms: None,
            retry_policy: None,
            next: None,
        });

        if depth == 0 {
            return leaf.boxed();
        }

        prop_oneof![
            // Task with optional next and optional timeout
            (
                arb_id(),
                prop::option::of(any::<u64>()),
                prop::option::of(arb_continuation(depth - 1).prop_map(Box::new)),
            )
                .prop_map(|(id, timeout_ms, next)| SerializableContinuation::Task {
                    id,
                    timeout_ms,
                    retry_policy: None,
                    next,
                }),
            // Fork with branches and optional join
            (
                arb_id(),
                prop::collection::vec(arb_continuation(depth - 1), 0..3),
                prop::option::of(arb_continuation(depth - 1).prop_map(Box::new)),
            )
                .prop_map(|(id, branches, join)| SerializableContinuation::Fork {
                    id,
                    branches,
                    join,
                }),
            // Delay with optional next
            (
                arb_id(),
                any::<u64>(),
                prop::option::of(arb_continuation(depth - 1).prop_map(Box::new)),
            )
                .prop_map(|(id, duration_ms, next)| SerializableContinuation::Delay {
                    id,
                    duration_ms,
                    next,
                }),
            // AwaitSignal with optional next
            (
                arb_id(),
                arb_id(),
                prop::option::of(any::<u64>()),
                prop::option::of(arb_continuation(depth - 1).prop_map(Box::new)),
            )
                .prop_map(|(id, signal_name, timeout_ms, next)| {
                    SerializableContinuation::AwaitSignal {
                        id,
                        signal_name,
                        timeout_ms,
                        next,
                    }
                }),
            // Branch with named branches, optional default and next
            (
                arb_id(),
                prop::collection::hash_map(
                    arb_id(),
                    arb_continuation(depth - 1).prop_map(Box::new),
                    0..3
                ),
                prop::option::of(arb_continuation(depth - 1).prop_map(Box::new)),
                prop::option::of(arb_continuation(depth - 1).prop_map(Box::new)),
            )
                .prop_map(|(id, branches, default, next)| {
                    SerializableContinuation::Branch {
                        id,
                        branches,
                        default,
                        next,
                    }
                }),
        ]
        .boxed()
    }

    /// Strategy for a continuation tree where all IDs are guaranteed unique.
    ///
    /// Each node gets an ID formed by its path index to prevent collisions.
    fn arb_unique_continuation(
        depth: usize,
        prefix: &str,
    ) -> BoxedStrategy<SerializableContinuation> {
        let id = format!("{prefix}n");

        if depth == 0 {
            return Just(SerializableContinuation::Task {
                id,
                timeout_ms: None,
                retry_policy: None,
                next: None,
            })
            .boxed();
        }

        let id_clone = id.clone();
        prop_oneof![
            // Task with optional next
            prop::option::of(
                arb_unique_continuation(depth - 1, &format!("{prefix}0_")).prop_map(Box::new),
            )
            .prop_map(move |next| SerializableContinuation::Task {
                id: id_clone.clone(),
                timeout_ms: None,
                retry_policy: None,
                next,
            }),
            // Fork with 0..3 branches (each gets unique prefix) and optional join
            {
                let id_f = id.clone();
                let prefix_f = prefix.to_string();
                (0..3u8)
                    .prop_flat_map(move |branch_count| {
                        let id_inner = id_f.clone();
                        let prefix_inner = prefix_f.clone();
                        let branches: Vec<BoxedStrategy<SerializableContinuation>> = (0
                            ..branch_count)
                            .map(|i| {
                                arb_unique_continuation(depth - 1, &format!("{prefix_inner}b{i}_"))
                            })
                            .collect();
                        let join = prop::option::of(
                            arb_unique_continuation(depth - 1, &format!("{prefix_inner}j_"))
                                .prop_map(Box::new),
                        );
                        (branches, join).prop_map(move |(branches, join)| {
                            SerializableContinuation::Fork {
                                id: id_inner.clone(),
                                branches,
                                join,
                            }
                        })
                    })
                    .boxed()
            },
            // Delay with optional next
            {
                let id_d = id.clone();
                let prefix_d = prefix.to_string();
                (
                    any::<u64>(),
                    prop::option::of(
                        arb_unique_continuation(depth - 1, &format!("{prefix_d}d_"))
                            .prop_map(Box::new),
                    ),
                )
                    .prop_map(move |(duration_ms, next)| {
                        SerializableContinuation::Delay {
                            id: id_d.clone(),
                            duration_ms,
                            next,
                        }
                    })
            },
            // AwaitSignal with optional next
            {
                let id_s = id.clone();
                let prefix_s = prefix.to_string();
                (
                    arb_id(),
                    prop::option::of(any::<u64>()),
                    prop::option::of(
                        arb_unique_continuation(depth - 1, &format!("{prefix_s}s_"))
                            .prop_map(Box::new),
                    ),
                )
                    .prop_map(move |(signal_name, timeout_ms, next)| {
                        SerializableContinuation::AwaitSignal {
                            id: id_s.clone(),
                            signal_name,
                            timeout_ms,
                            next,
                        }
                    })
            },
            // Branch with two named branches, optional default and next
            {
                let id_b = id;
                let prefix_b = prefix.to_string();
                let b0 = arb_unique_continuation(depth - 1, &format!("{prefix_b}br0_"))
                    .prop_map(Box::new);
                let b1 = arb_unique_continuation(depth - 1, &format!("{prefix_b}br1_"))
                    .prop_map(Box::new);
                let default = prop::option::of(
                    arb_unique_continuation(depth - 1, &format!("{prefix_b}bd_"))
                        .prop_map(Box::new),
                );
                let next = prop::option::of(
                    arb_unique_continuation(depth - 1, &format!("{prefix_b}bn_"))
                        .prop_map(Box::new),
                );
                (b0, b1, default, next).prop_map(move |(branch0, branch1, default, next)| {
                    let mut branches = std::collections::HashMap::new();
                    branches.insert("k0".to_string(), branch0);
                    branches.insert("k1".to_string(), branch1);
                    SerializableContinuation::Branch {
                        id: id_b.clone(),
                        branches,
                        default,
                        next,
                    }
                })
            },
        ]
        .boxed()
    }

    /// Collect all IDs in a continuation tree.
    fn collect_ids(cont: &SerializableContinuation) -> Vec<String> {
        let mut ids = Vec::new();
        fn walk(c: &SerializableContinuation, out: &mut Vec<String>) {
            match c {
                SerializableContinuation::Task { id, next, .. }
                | SerializableContinuation::Delay { id, next, .. }
                | SerializableContinuation::AwaitSignal { id, next, .. } => {
                    out.push(id.clone());
                    if let Some(n) = next {
                        walk(n, out);
                    }
                }
                SerializableContinuation::Fork { id, branches, join } => {
                    out.push(id.clone());
                    for b in branches {
                        walk(b, out);
                    }
                    if let Some(j) = join {
                        walk(j, out);
                    }
                }
                SerializableContinuation::Branch {
                    id,
                    branches,
                    default,
                    next,
                } => {
                    out.push(id.clone());
                    for b in branches.values() {
                        walk(b, out);
                    }
                    if let Some(d) = default {
                        walk(d, out);
                    }
                    if let Some(n) = next {
                        walk(n, out);
                    }
                }
            }
        }
        walk(cont, &mut ids);
        ids
    }

    /// Inject a duplicate ID into a continuation by replacing the first node's ID.
    fn inject_duplicate(cont: &SerializableContinuation, dup_id: &str) -> SerializableContinuation {
        match cont {
            SerializableContinuation::Task {
                timeout_ms,
                retry_policy,
                next,
                ..
            } => SerializableContinuation::Task {
                id: dup_id.to_string(),
                timeout_ms: *timeout_ms,
                retry_policy: retry_policy.clone(),
                next: next.clone(),
            },
            SerializableContinuation::Fork { branches, join, .. } => {
                SerializableContinuation::Fork {
                    id: dup_id.to_string(),
                    branches: branches.clone(),
                    join: join.clone(),
                }
            }
            SerializableContinuation::Delay {
                duration_ms, next, ..
            } => SerializableContinuation::Delay {
                id: dup_id.to_string(),
                duration_ms: *duration_ms,
                next: next.clone(),
            },
            SerializableContinuation::AwaitSignal {
                signal_name,
                timeout_ms,
                next,
                ..
            } => SerializableContinuation::AwaitSignal {
                id: dup_id.to_string(),
                signal_name: signal_name.clone(),
                timeout_ms: *timeout_ms,
                next: next.clone(),
            },
            SerializableContinuation::Branch {
                branches,
                default,
                next,
                ..
            } => SerializableContinuation::Branch {
                id: dup_id.to_string(),
                branches: branches.clone(),
                default: default.clone(),
                next: next.clone(),
            },
        }
    }

    proptest! {
        // Property 4: `compute_definition_hash` is deterministic.
        #[test]
        fn hash_is_deterministic(cont in arb_continuation(3)) {
            let h1 = cont.compute_definition_hash();
            let h2 = cont.compute_definition_hash();
            prop_assert_eq!(h1, h2);
        }

        // Property 5: serde roundtrip preserves the definition hash.
        #[test]
        fn serde_roundtrip_preserves_hash(cont in arb_continuation(3)) {
            let original_hash = cont.compute_definition_hash();
            let json = serde_json::to_string(&cont).unwrap();
            let recovered: SerializableContinuation = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(original_hash, recovered.compute_definition_hash());
        }

        // Property 6: a tree with guaranteed-unique IDs has no duplicates.
        #[test]
        fn unique_ids_means_none(cont in arb_unique_continuation(3, "r_")) {
            prop_assert!(cont.find_duplicate_id().is_none());
        }

        // Property 7: injecting a duplicate ID is always detected.
        #[test]
        fn injected_duplicate_is_detected(cont in arb_unique_continuation(3, "r_")) {
            let ids = collect_ids(&cont);
            // Need at least 2 nodes to have a meaningful duplicate injection
            if ids.len() >= 2 {
                // Pick the second ID and inject it into the root (which has the first ID)
                let dup_id = &ids[1];
                let tampered = inject_duplicate(&cont, dup_id);
                prop_assert!(tampered.find_duplicate_id().is_some());
            }
        }
    }
}
