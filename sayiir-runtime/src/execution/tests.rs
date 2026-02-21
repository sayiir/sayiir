// Lint allows are on the `mod tests` declaration in mod.rs.

use super::*;
use crate::error::RuntimeError;
use crate::serialization::JsonCodec;
use bytes::Bytes;
use sayiir_core::branch_results::NamedBranchResults;
use sayiir_core::error::{BoxError, WorkflowError};
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use sayiir_core::task::{RetryPolicy, to_core_task};
use sayiir_core::workflow::{WorkflowContinuation, WorkflowStatus};
use sayiir_persistence::{InMemoryBackend, SignalStore, SnapshotStore};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

fn codec() -> Arc<JsonCodec> {
    Arc::new(JsonCodec)
}

fn encode_u32(val: u32) -> Bytes {
    Bytes::from(serde_json::to_vec(&val).unwrap())
}

fn decode_u32(bytes: &Bytes) -> u32 {
    serde_json::from_slice(bytes).unwrap()
}

/// Build a `WorkflowContinuation::Task` with a real func.
fn task_node<F, Fut>(
    id: &str,
    f: F,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation
where
    F: Fn(u32) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<u32, BoxError>> + Send + 'static,
{
    let c = codec();
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: Some(to_core_task(id, f, c)),
        timeout: None,
        retry_policy: None,
        version: None,
        next,
    }
}

/// Build a `WorkflowContinuation::Task` with no func (for callback-based tests).
fn stub_node(id: &str, next: Option<Box<WorkflowContinuation>>) -> WorkflowContinuation {
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: None,
        timeout: None,
        retry_policy: None,
        version: None,
        next,
    }
}

/// Build a task node with a retry policy (async with real func).
fn task_node_with_retry<F, Fut>(
    id: &str,
    f: F,
    retry_policy: RetryPolicy,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation
where
    F: Fn(u32) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<u32, BoxError>> + Send + 'static,
{
    let c = codec();
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: Some(to_core_task(id, f, c)),
        timeout: None,
        retry_policy: Some(retry_policy),
        version: None,
        next,
    }
}

/// Build a task node with both timeout and retry policy.
fn task_node_with_timeout_and_retry<F, Fut>(
    id: &str,
    f: F,
    timeout: std::time::Duration,
    retry_policy: RetryPolicy,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation
where
    F: Fn(u32) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<u32, BoxError>> + Send + 'static,
{
    let c = codec();
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: Some(to_core_task(id, f, c)),
        timeout: Some(timeout),
        retry_policy: Some(retry_policy),
        version: None,
        next,
    }
}

/// Build a stub node with a retry policy (callback-based tests).
fn stub_node_with_retry(
    id: &str,
    retry_policy: RetryPolicy,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation {
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: None,
        timeout: None,
        retry_policy: Some(retry_policy),
        version: None,
        next,
    }
}

/// Build a stub node with timeout + retry policy (callback-based tests).
fn stub_node_with_timeout_and_retry(
    id: &str,
    timeout: std::time::Duration,
    retry_policy: RetryPolicy,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation {
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: None,
        timeout: Some(timeout),
        retry_policy: Some(retry_policy),
        version: None,
        next,
    }
}

/// A fast retry policy for tests: minimal delays, configurable max retries.
fn fast_retry(max_retries: u32) -> RetryPolicy {
    RetryPolicy {
        max_retries,
        initial_delay: std::time::Duration::from_millis(1),
        backoff_multiplier: 1.0,
        max_delay: None,
    }
}

// ========================================================================
// serialize_branch_results
// ========================================================================

#[test]
fn test_serialize_branch_results_roundtrip() {
    let results = vec![
        ("branch_a".to_string(), Bytes::from(vec![1, 2, 3])),
        ("branch_b".to_string(), Bytes::from(vec![4, 5])),
    ];

    let serialized = serialize_branch_results(&results, &JsonCodec).unwrap();
    let deserialized: NamedBranchResults = serde_json::from_slice(&serialized).unwrap();
    let map = deserialized.into_map();

    assert_eq!(map.len(), 2);
    assert_eq!(map["branch_a"], Bytes::from(vec![1, 2, 3]));
    assert_eq!(map["branch_b"], Bytes::from(vec![4, 5]));
}

#[test]
fn test_serialize_branch_results_empty() {
    let results: Vec<(String, Bytes)> = vec![];
    let serialized = serialize_branch_results(&results, &JsonCodec).unwrap();
    let deserialized: NamedBranchResults = serde_json::from_slice(&serialized).unwrap();
    assert!(deserialized.is_empty());
}

#[test]
fn test_serialize_branch_results_single() {
    let results = vec![("only".to_string(), Bytes::from("data"))];
    let serialized = serialize_branch_results(&results, &JsonCodec).unwrap();
    let deserialized: NamedBranchResults = serde_json::from_slice(&serialized).unwrap();
    let map = deserialized.into_map();
    assert_eq!(map.len(), 1);
    assert_eq!(map["only"], Bytes::from("data"));
}

// ========================================================================
// execute_continuation_sync
// ========================================================================

#[test]
fn test_sync_single_task() {
    let input = encode_u32(5);
    let cont = stub_node("add_one", None);

    let callback = |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        Ok(encode_u32(val + 1))
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 6);
}

#[test]
fn test_sync_chained_tasks() {
    let double = stub_node("double", None);
    let add_one = stub_node("add_one", Some(Box::new(double)));
    let input = encode_u32(10);

    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "add_one" => Ok(encode_u32(val + 1)),
            "double" => Ok(encode_u32(val * 2)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&add_one, input, &callback, &JsonCodec).unwrap();
    // 10 + 1 = 11, 11 * 2 = 22
    assert_eq!(decode_u32(&result), 22);
}

#[test]
fn test_sync_fork_with_join() {
    let branch_a = Arc::new(stub_node("branch_a", None));
    let branch_b = Arc::new(stub_node("branch_b", None));
    let join_task = stub_node("join", None);

    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_task)),
    };

    let input = encode_u32(10);

    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val: u32 = serde_json::from_slice(&input).unwrap_or(0);
        match id {
            "branch_a" => Ok(encode_u32(val * 2)),
            "branch_b" => Ok(encode_u32(val + 5)),
            "join" => {
                let branches: NamedBranchResults = serde_json::from_slice(&input).unwrap();
                let map = branches.into_map();
                let a = decode_u32(&map["branch_a"]);
                let b = decode_u32(&map["branch_b"]);
                Ok(encode_u32(a + b))
            }
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&fork, input, &callback, &JsonCodec).unwrap();
    // branch_a: 10*2=20, branch_b: 10+5=15, join: 20+15=35
    assert_eq!(decode_u32(&result), 35);
}

#[test]
fn test_sync_fork_without_join() {
    let branch_a = Arc::new(stub_node("branch_a", None));
    let branch_b = Arc::new(stub_node("branch_b", None));

    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: None,
    };

    let input = encode_u32(10);

    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "branch_a" => Ok(encode_u32(val * 2)),
            "branch_b" => Ok(encode_u32(val + 5)),
            _ => Err("Unknown".into()),
        }
    };

    // Without join, returns last branch result
    let result = execute_continuation_sync(&fork, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 15); // branch_b: 10+5
}

#[test]
fn test_sync_task_failure_propagates() {
    let cont = stub_node("fail_task", None);
    let input = encode_u32(1);

    let callback =
        |_id: &str, _input: Bytes| -> Result<Bytes, BoxError> { Err("task exploded".into()) };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("task exploded"));
}

// ========================================================================
// execute_continuation_async
// ========================================================================

#[tokio::test]
async fn test_async_single_task() {
    let input = encode_u32(5);
    let cont = task_node("add_one", |i: u32| async move { Ok(i + 1) }, None);

    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 6);
}

#[tokio::test]
async fn test_async_chained_tasks() {
    let double = task_node("double", |i: u32| async move { Ok(i * 2) }, None);
    let add_one = task_node(
        "add_one",
        |i: u32| async move { Ok(i + 1) },
        Some(Box::new(double)),
    );

    let input = encode_u32(10);
    let result = execute_continuation_async(&add_one, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 22);
}

#[tokio::test]
async fn test_async_fork_with_parallel_branches() {
    let branch_a = Arc::new(task_node(
        "branch_a",
        |i: u32| async move { Ok(i * 2) },
        None,
    ));
    let branch_b = Arc::new(task_node(
        "branch_b",
        |i: u32| async move { Ok(i + 5) },
        None,
    ));

    // No join - returns last branch result
    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: None,
    };

    let input = encode_u32(10);
    let result = execute_continuation_async(&fork, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 15); // branch_b: 10+5
}

#[tokio::test]
async fn test_async_task_no_implementation() {
    let cont = WorkflowContinuation::Task {
        id: "missing".into(),
        func: None,
        timeout: None,
        retry_policy: None,
        version: None,
        next: None,
    };

    let result = execute_continuation_async(&cont, Bytes::new(), &JsonCodec).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("no implementation")
    );
}

#[tokio::test]
async fn test_async_task_failure_propagates() {
    let cont = task_node(
        "fail",
        |_i: u32| async move { Err::<u32, BoxError>("async task failed".into()) },
        None,
    );

    let input = encode_u32(1);
    let result = execute_continuation_async(&cont, input, &JsonCodec).await;
    assert!(result.is_err());
}

// ========================================================================
// Task timeout tests
// ========================================================================

#[tokio::test]
async fn test_async_task_completes_within_timeout() {
    let cont = WorkflowContinuation::Task {
        id: "fast".to_string(),
        func: Some(to_core_task(
            "fast",
            |i: u32| async move { Ok(i + 1) },
            codec(),
        )),
        timeout: Some(std::time::Duration::from_secs(5)),
        retry_policy: None,
        version: None,
        next: None,
    };

    let input = encode_u32(10);
    let result = execute_continuation_async(&cont, input, &JsonCodec).await;
    assert!(result.is_ok());
    assert_eq!(decode_u32(&result.unwrap()), 11);
}

#[tokio::test]
async fn test_async_task_exceeds_timeout() {
    let cont = WorkflowContinuation::Task {
        id: "slow".to_string(),
        func: Some(to_core_task(
            "slow",
            |i: u32| async move {
                // Sleep just long enough to exceed the timeout
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(i + 1)
            },
            codec(),
        )),
        // Deadline shorter than the sleep — post-execution check will fail
        timeout: Some(std::time::Duration::from_millis(5)),
        retry_policy: None,
        version: None,
        next: None,
    };

    let input = encode_u32(10);
    let result = execute_continuation_async(&cont, input, &JsonCodec).await;
    let err = result.unwrap_err();
    assert!(err.to_string().contains("timed out"));
    assert!(err.to_string().contains("slow"));
}

#[tokio::test]
async fn test_async_task_no_timeout_unlimited() {
    // Task with no timeout should complete regardless of runtime
    let cont = WorkflowContinuation::Task {
        id: "normal".to_string(),
        func: Some(to_core_task(
            "normal",
            |i: u32| async move {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                Ok(i + 1)
            },
            codec(),
        )),
        timeout: None,
        retry_policy: None,
        version: None,
        next: None,
    };

    let input = encode_u32(42);
    let result = execute_continuation_async(&cont, input, &JsonCodec).await;
    assert!(result.is_ok());
    assert_eq!(decode_u32(&result.unwrap()), 43);
}

#[tokio::test]
async fn test_checkpointing_task_timeout() {
    let backend = InMemoryBackend::new();
    let cont = WorkflowContinuation::Task {
        id: "slow".to_string(),
        func: None,
        timeout: Some(std::time::Duration::from_millis(10)),
        retry_policy: None,
        version: None,
        next: None,
    };

    let input = encode_u32(1);
    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "slow".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let slow_task = |_id: &str, input: Bytes| async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        Ok(input)
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &slow_task,
        &JsonCodec,
    )
    .await;

    let err = result.unwrap_err();
    assert!(err.to_string().contains("timed out"));
    assert!(err.to_string().contains("slow"));
}

#[tokio::test]
async fn test_checkpointing_skipped_tasks_bypass_timeout() {
    let backend = InMemoryBackend::new();
    // Task with very short timeout — but it's already cached, so timeout shouldn't matter
    let cont = WorkflowContinuation::Task {
        id: "cached".to_string(),
        func: None,
        timeout: Some(std::time::Duration::from_millis(1)),
        retry_policy: None,
        version: None,
        next: None,
    };

    let output = encode_u32(42);
    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), encode_u32(1));
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "cached".into(),
    });
    snapshot.mark_task_completed("cached".to_string(), output.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let never_called = |_id: &str, _input: Bytes| async move {
        panic!("should not be called for cached tasks");
        #[allow(unreachable_code)]
        Ok(Bytes::new())
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        encode_u32(1),
        &mut snapshot,
        &backend,
        &never_called,
        &JsonCodec,
    )
    .await;

    assert!(result.is_ok());
    assert_eq!(decode_u32(&result.unwrap()), 42);
}

// ========================================================================
// prepare_run / prepare_resume / finalize_execution
// ========================================================================

#[tokio::test]
async fn test_prepare_run_creates_snapshot() {
    let backend = InMemoryBackend::new();
    let snapshot = prepare_run(
        "inst-1".into(),
        "hash-1".into(),
        Bytes::from("input"),
        "task-1".into(),
        &backend,
    )
    .await
    .unwrap();

    assert_eq!(snapshot.instance_id, "inst-1");
    assert_eq!(snapshot.definition_hash, "hash-1");
    assert!(snapshot.state.is_in_progress());

    // Verify it was saved to backend
    let loaded = backend.load_snapshot("inst-1").await.unwrap();
    assert_eq!(loaded.instance_id, "inst-1");
}

#[tokio::test]
async fn test_prepare_resume_ready() {
    let backend = InMemoryBackend::new();
    let snapshot = WorkflowSnapshot::with_initial_input(
        "inst-1".into(),
        "hash-1".into(),
        Bytes::from("input"),
    );
    backend.save_snapshot(&snapshot).await.unwrap();

    let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
    match outcome {
        ResumeOutcome::Ready {
            snapshot,
            input_bytes,
        } => {
            assert_eq!(snapshot.instance_id, "inst-1");
            assert_eq!(input_bytes, Bytes::from("input"));
        }
        _ => panic!("Expected Ready outcome"),
    }
}

#[tokio::test]
async fn test_prepare_resume_with_completed_tasks() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::with_initial_input(
        "inst-1".into(),
        "hash-1".into(),
        Bytes::from("initial"),
    );
    snapshot.mark_task_completed("task-1".into(), Bytes::from("task1_output"));
    backend.save_snapshot(&snapshot).await.unwrap();

    let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
    match outcome {
        ResumeOutcome::Ready { input_bytes, .. } => {
            // Should use last task output, not initial input
            assert_eq!(input_bytes, Bytes::from("task1_output"));
        }
        _ => panic!("Expected Ready outcome"),
    }
}

#[tokio::test]
async fn test_prepare_resume_already_completed() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    snapshot.mark_completed(Bytes::from("result"));
    backend.save_snapshot(&snapshot).await.unwrap();

    let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
    match outcome {
        ResumeOutcome::AlreadyTerminal(WorkflowStatus::Completed) => {}
        _ => panic!("Expected AlreadyTerminal(Completed)"),
    }
}

#[tokio::test]
async fn test_prepare_resume_already_failed() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    snapshot.mark_failed("err".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
    match outcome {
        ResumeOutcome::AlreadyTerminal(WorkflowStatus::Failed(_)) => {}
        _ => panic!("Expected AlreadyTerminal(Failed)"),
    }
}

#[tokio::test]
async fn test_prepare_resume_already_cancelled() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    snapshot.mark_cancelled(Some("reason".into()), Some("admin".into()), None);
    backend.save_snapshot(&snapshot).await.unwrap();

    let outcome = prepare_resume("inst-1", "hash-1", &backend).await.unwrap();
    match outcome {
        ResumeOutcome::AlreadyTerminal(WorkflowStatus::Cancelled { reason, .. }) => {
            assert_eq!(reason, Some("reason".into()));
        }
        _ => panic!("Expected AlreadyTerminal(Cancelled)"),
    }
}

#[tokio::test]
async fn test_prepare_resume_hash_mismatch() {
    let backend = InMemoryBackend::new();
    let snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    let result = prepare_resume("inst-1", "wrong-hash", &backend).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("mismatch"));
}

#[tokio::test]
async fn test_finalize_execution_success() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    let (status, output) = finalize_execution(Ok(Bytes::from("output")), &mut snapshot, &backend)
        .await
        .unwrap();

    match status {
        WorkflowStatus::Completed => {}
        _ => panic!("Expected Completed"),
    }
    assert_eq!(output, Some(Bytes::from("output")));

    let saved = backend.load_snapshot("inst-1").await.unwrap();
    assert!(saved.state.is_completed());
}

#[tokio::test]
async fn test_finalize_execution_failure() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    backend.save_snapshot(&snapshot).await.unwrap();

    let (status, output) = finalize_execution(
        Err(RuntimeError::from(BoxError::from("task failed"))),
        &mut snapshot,
        &backend,
    )
    .await
    .unwrap();

    match status {
        WorkflowStatus::Failed(e) => {
            assert!(e.contains("task failed"));
        }
        _ => panic!("Expected Failed"),
    }
    assert!(output.is_none());

    let saved = backend.load_snapshot("inst-1").await.unwrap();
    assert!(saved.state.is_failed());
}

#[tokio::test]
async fn test_finalize_execution_cancellation() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    // Mark as cancelled in backend so finalize can reload details
    snapshot.mark_cancelled(Some("timeout".into()), Some("system".into()), None);
    backend.save_snapshot(&snapshot).await.unwrap();

    // Reset local snapshot to in-progress for finalize logic
    let mut local_snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());

    let (status, output) = finalize_execution(
        Err(WorkflowError::cancelled().into()),
        &mut local_snapshot,
        &backend,
    )
    .await
    .unwrap();

    match status {
        WorkflowStatus::Cancelled {
            reason,
            cancelled_by,
        } => {
            assert_eq!(reason, Some("timeout".into()));
            assert_eq!(cancelled_by, Some("system".into()));
        }
        _ => panic!("Expected Cancelled"),
    }
    assert!(output.is_none());
}

// ========================================================================
// execute_continuation_with_checkpointing
// ========================================================================

#[tokio::test]
async fn test_checkpointing_single_task() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let cont = stub_node("add_one", None);

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val: u32 = serde_json::from_slice(&input)?;
            match id.as_str() {
                "add_one" => Ok(Bytes::from(serde_json::to_vec(&(val + 1))?)),
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 6);
    assert!(snapshot.get_task_result("add_one").is_some());
}

#[tokio::test]
async fn test_checkpointing_chain() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let double = stub_node("double", None);
    let add_one = stub_node("add_one", Some(Box::new(double)));

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val: u32 = serde_json::from_slice(&input)?;
            match id.as_str() {
                "add_one" => Ok(Bytes::from(serde_json::to_vec(&(val + 1))?)),
                "double" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &add_one,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 22); // (10+1)*2
    assert!(snapshot.get_task_result("add_one").is_some());
    assert!(snapshot.get_task_result("double").is_some());
}

#[tokio::test]
async fn test_checkpointing_skips_completed_tasks() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    // Pre-mark task as completed (simulates resume)
    snapshot.mark_task_completed("add_one".into(), encode_u32(11));
    backend.save_snapshot(&snapshot).await.unwrap();

    let double = stub_node("double", None);
    let add_one = stub_node("add_one", Some(Box::new(double)));

    let was_called = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let was_called_clone = was_called.clone();

    let callback = move |id: &str, input: Bytes| {
        let id = id.to_string();
        let was_called_inner = was_called_clone.clone();
        async move {
            let val: u32 = serde_json::from_slice(&input)?;
            match id.as_str() {
                "add_one" => {
                    was_called_inner.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(Bytes::from(serde_json::to_vec(&(val + 1))?))
                }
                "double" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &add_one,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // add_one should NOT have been called - it was already completed
    assert!(!was_called.load(std::sync::atomic::Ordering::SeqCst));
    // cached output 11 * 2 = 22
    assert_eq!(decode_u32(&result), 22);
}

#[tokio::test]
async fn test_checkpointing_fork_sequential() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let branch_a = Arc::new(stub_node("branch_a", None));
    let branch_b = Arc::new(stub_node("branch_b", None));
    let join_task = stub_node("join", None);

    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_task)),
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val: u32 = serde_json::from_slice(&input).unwrap_or(0);
            match id.as_str() {
                "branch_a" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                "branch_b" => Ok(Bytes::from(serde_json::to_vec(&(val + 5))?)),
                "join" => {
                    let branches: NamedBranchResults = serde_json::from_slice(&input)?;
                    let map = branches.into_map();
                    let a: u32 = serde_json::from_slice(&map["branch_a"])?;
                    let b: u32 = serde_json::from_slice(&map["branch_b"])?;
                    Ok(Bytes::from(serde_json::to_vec(&(a + b))?))
                }
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &fork,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // branch_a: 10*2=20, branch_b: 10+5=15, join: 20+15=35
    assert_eq!(decode_u32(&result), 35);
}

#[tokio::test]
async fn test_checkpointing_cancellation() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    // Request cancellation before execution
    backend
        .store_signal(
            "inst-1",
            sayiir_core::snapshot::SignalKind::Cancel,
            sayiir_core::snapshot::SignalRequest::new(
                Some("test cancel".into()),
                Some("tester".into()),
            ),
        )
        .await
        .unwrap();

    let cont = stub_node("task1", None);

    let callback =
        |_id: &str, _input: Bytes| async { Err::<Bytes, BoxError>("Should not be called".into()) };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::Workflow(WorkflowError::Cancelled { .. })
    ));
}

// ========================================================================
// get_resume_input
// ========================================================================

#[test]
fn test_get_resume_input_no_completed_tasks() {
    let snapshot = WorkflowSnapshot::with_initial_input(
        "inst-1".into(),
        "hash-1".into(),
        Bytes::from("initial"),
    );
    let input = get_resume_input(&snapshot).unwrap();
    assert_eq!(input, Bytes::from("initial"));
}

#[test]
fn test_get_resume_input_with_completed_tasks() {
    let mut snapshot = WorkflowSnapshot::with_initial_input(
        "inst-1".into(),
        "hash-1".into(),
        Bytes::from("initial"),
    );
    snapshot.mark_task_completed("task-1".into(), Bytes::from("task1_out"));
    let input = get_resume_input(&snapshot).unwrap();
    assert_eq!(input, Bytes::from("task1_out"));
}

#[test]
fn test_get_resume_input_not_in_progress() {
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    snapshot.mark_completed(Bytes::from("done"));
    let result = get_resume_input(&snapshot);
    assert!(result.is_err());
}

#[test]
fn test_get_resume_input_no_initial_input() {
    let snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());
    let result = get_resume_input(&snapshot);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("initial input not stored")
    );
}

// ========================================================================
// Delay tests
// ========================================================================

#[test]
fn test_sync_delay_passthrough() {
    let delay = WorkflowContinuation::Delay {
        id: "short_wait".into(),
        duration: std::time::Duration::from_millis(1),
        next: None,
    };

    let input = encode_u32(42);
    let callback = |_id: &str, _input: Bytes| -> Result<Bytes, BoxError> {
        panic!("callback should not be called for delay");
    };

    let result = execute_continuation_sync(&delay, input, &callback, &JsonCodec).unwrap();
    // Delay passes input through unchanged
    assert_eq!(decode_u32(&result), 42);
}

#[test]
fn test_sync_delay_in_chain() {
    let double = stub_node("double", None);
    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(double)),
    };
    let add_one = stub_node("add_one", Some(Box::new(delay)));

    let input = encode_u32(10);
    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "add_one" => Ok(encode_u32(val + 1)),
            "double" => Ok(encode_u32(val * 2)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&add_one, input, &callback, &JsonCodec).unwrap();
    // 10 + 1 = 11, delay (passthrough 11), 11 * 2 = 22
    assert_eq!(decode_u32(&result), 22);
}

#[tokio::test]
async fn test_async_delay_passthrough() {
    let delay = WorkflowContinuation::Delay {
        id: "short_wait".into(),
        duration: std::time::Duration::from_millis(1),
        next: None,
    };

    let input = encode_u32(99);
    let result = execute_continuation_async(&delay, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 99);
}

#[tokio::test]
async fn test_async_delay_in_chain() {
    let double = task_node("double", |i: u32| async move { Ok(i * 2) }, None);
    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(double)),
    };
    let add_one = task_node(
        "add_one",
        |i: u32| async move { Ok(i + 1) },
        Some(Box::new(delay)),
    );

    let input = encode_u32(5);
    let result = execute_continuation_async(&add_one, input, &JsonCodec)
        .await
        .unwrap();
    // 5 + 1 = 6, delay (passthrough 6), 6 * 2 = 12
    assert_eq!(decode_u32(&result), 12);
}

#[tokio::test]
async fn test_checkpointing_delay_returns_waiting() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(42);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let next_task = stub_node("process", None);
    let delay = WorkflowContinuation::Delay {
        id: "wait_1h".into(),
        duration: std::time::Duration::from_secs(3600),
        next: Some(Box::new(next_task)),
    };

    let callback =
        |_id: &str, _input: Bytes| async { Err::<Bytes, BoxError>("Should not be called".into()) };

    let result = execute_continuation_with_checkpointing(
        &delay,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    // Should return a Waiting error
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));

    // Snapshot should be at AtDelay position with pass-through stored
    match &snapshot.state {
        WorkflowSnapshotState::InProgress { position, .. } => match position {
            ExecutionPosition::AtDelay {
                delay_id,
                next_task_id,
                ..
            } => {
                assert_eq!(delay_id, "wait_1h");
                assert_eq!(next_task_id.as_deref(), Some("process"));
            }
            other => panic!("Expected AtDelay, got {other:?}"),
        },
        other => panic!("Expected InProgress, got {other:?}"),
    }

    // Pass-through value should be stored
    let stored = snapshot.get_task_result("wait_1h").unwrap();
    assert_eq!(decode_u32(&stored.output), 42);
}

#[tokio::test]
async fn test_checkpointing_delay_skip_on_resume() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(42);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    // Pre-mark delay as completed (simulates resume after delay expired)
    snapshot.mark_task_completed("wait".into(), encode_u32(42));
    backend.save_snapshot(&snapshot).await.unwrap();

    let process = stub_node("process", None);
    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_secs(3600),
        next: Some(Box::new(process)),
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val: u32 = serde_json::from_slice(&input)?;
            match id.as_str() {
                "process" => Ok(Bytes::from(serde_json::to_vec(&(val + 10))?)),
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &delay,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // Delay was skipped (already completed), process received 42, output 52
    assert_eq!(decode_u32(&result), 52);
}

#[tokio::test]
async fn test_checkpointing_delay_cancellation() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    // Request cancellation
    backend
        .store_signal(
            "inst-1",
            sayiir_core::snapshot::SignalKind::Cancel,
            sayiir_core::snapshot::SignalRequest::new(
                Some("test cancel".into()),
                Some("tester".into()),
            ),
        )
        .await
        .unwrap();

    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_secs(3600),
        next: None,
    };

    let callback =
        |_id: &str, _input: Bytes| async { Err::<Bytes, BoxError>("Should not be called".into()) };

    let result = execute_continuation_with_checkpointing(
        &delay,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Cancelled { .. })
    ));
}

#[tokio::test]
async fn test_finalize_execution_waiting() {
    let backend = InMemoryBackend::new();
    let mut snapshot = WorkflowSnapshot::new("inst-1".into(), "hash-1".into());

    // Set up the snapshot as if it's parked at a delay
    let now = chrono::Utc::now();
    let wake_at = now + chrono::Duration::hours(1);
    snapshot.update_position(ExecutionPosition::AtDelay {
        delay_id: "my_delay".into(),
        entered_at: now,
        wake_at,
        next_task_id: Some("next_step".into()),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let (status, output) = finalize_execution(
        Err(WorkflowError::Waiting { wake_at }.into()),
        &mut snapshot,
        &backend,
    )
    .await
    .unwrap();

    match status {
        WorkflowStatus::Waiting {
            wake_at: wa,
            delay_id,
        } => {
            assert_eq!(wa, wake_at);
            assert_eq!(delay_id, "my_delay");
        }
        _ => panic!("Expected Waiting status, got {status:?}"),
    }
    assert!(output.is_none());

    // Snapshot should still be in-progress (not completed or failed)
    let loaded = backend.load_snapshot("inst-1").await.unwrap();
    assert!(loaded.state.is_in_progress());
}

// ========================================================================
// Fork with delay (durable delays inside branches)
// ========================================================================

/// Helper: build a fork with a delay inside one branch.
///
/// Structure: `Fork(branch_a: task, branch_b: task -> delay -> task) -> join`
fn fork_with_delay_in_branch() -> WorkflowContinuation {
    let branch_a = Arc::new(stub_node("branch_a", None));

    let after_delay = stub_node("after_delay", None);
    let delay = WorkflowContinuation::Delay {
        id: "branch_delay".into(),
        duration: std::time::Duration::from_secs(3600),
        next: Some(Box::new(after_delay)),
    };
    let branch_b = Arc::new(stub_node("before_delay", Some(Box::new(delay))));

    let join_task = stub_node("join", None);

    WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_task)),
    }
}

type BoxFut = std::pin::Pin<Box<dyn std::future::Future<Output = Result<Bytes, BoxError>> + Send>>;

fn make_fork_callback() -> impl Fn(&str, Bytes) -> BoxFut + Send + Sync {
    |id: &str, input: Bytes| {
        let id = id.to_string();
        Box::pin(async move {
            let val: u32 = serde_json::from_slice(&input).unwrap_or(0);
            match id.as_str() {
                "branch_a" => Ok(encode_u32(val * 2)),
                "before_delay" => Ok(encode_u32(val + 100)),
                "after_delay" => Ok(encode_u32(val + 1)),
                "join" => {
                    let branches: sayiir_core::branch_results::NamedBranchResults =
                        serde_json::from_slice(&input)?;
                    let map = branches.into_map();
                    let a: u32 = serde_json::from_slice(&map["branch_a"])?;
                    // branch_b's ID is "before_delay" (first task in the chain)
                    let b: u32 = serde_json::from_slice(&map["before_delay"])?;
                    Ok(encode_u32(a + b))
                }
                other => Err(format!("Unknown task: {other}").into()),
            }
        })
    }
}

#[tokio::test]
async fn test_fork_with_delay_parks_at_fork() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let fork = fork_with_delay_in_branch();
    let callback = make_fork_callback();

    let result = execute_continuation_with_checkpointing(
        &fork,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    // Should return Waiting because branch_b hits a delay
    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));

    // Snapshot should be at AtFork position
    match &snapshot.state {
        WorkflowSnapshotState::InProgress { position, .. } => match position {
            ExecutionPosition::AtFork {
                fork_id,
                completed_branches,
                wake_at,
            } => {
                assert_eq!(fork_id, "fork");
                // branch_a completed, branch_b is waiting
                assert_eq!(completed_branches.len(), 1);
                assert!(completed_branches.contains_key("branch_a"));
                assert!(*wake_at > chrono::Utc::now());
            }
            other => panic!("Expected AtFork, got {other:?}"),
        },
        other => panic!("Expected InProgress, got {other:?}"),
    }

    // branch_a result should be cached
    assert!(snapshot.get_task_result("branch_a").is_some());
    // before_delay task result should be cached (saved during branch execution)
    assert!(snapshot.get_task_result("before_delay").is_some());
    // delay pass-through should be cached
    assert!(snapshot.get_task_result("branch_delay").is_some());
}

#[tokio::test]
async fn test_fork_with_delay_resumes_after_expiry() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    // Use a very short delay so it expires immediately
    let branch_a = Arc::new(stub_node("branch_a", None));
    let after_delay = stub_node("after_delay", None);
    let delay = WorkflowContinuation::Delay {
        id: "branch_delay".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(after_delay)),
    };
    let branch_b = Arc::new(stub_node("before_delay", Some(Box::new(delay))));
    let join_task = stub_node("join", None);
    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_task)),
    };

    let callback = make_fork_callback();

    // First execution — parks at fork
    let result = execute_continuation_with_checkpointing(
        &fork,
        input.clone(),
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));

    // Wait for delay to expire
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Reload snapshot from backend (simulates what resume() does)
    snapshot = backend.load_snapshot("inst-1").await.unwrap();

    // Re-execute — cached tasks and expired delay should be skipped
    let result = execute_continuation_with_checkpointing(
        &fork,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    assert!(
        result.is_ok(),
        "Expected Ok after delay expired, got {result:?}"
    );
}

#[tokio::test]
async fn test_fork_with_delays_in_multiple_branches() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    // Both branches have delays
    let after_delay_a = stub_node("after_a", None);
    let delay_a = WorkflowContinuation::Delay {
        id: "delay_a".into(),
        duration: std::time::Duration::from_secs(100),
        next: Some(Box::new(after_delay_a)),
    };
    let branch_a = Arc::new(delay_a);

    let after_delay_b = stub_node("after_b", None);
    let delay_b = WorkflowContinuation::Delay {
        id: "delay_b".into(),
        duration: std::time::Duration::from_secs(200),
        next: Some(Box::new(after_delay_b)),
    };
    let branch_b = Arc::new(delay_b);

    let join_task = stub_node("join", None);
    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_task)),
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            match id.as_str() {
                "after_a" | "after_b" | "join" => Ok(input),
                other => Err(format!("Unknown task: {other}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &fork,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    // Should return Waiting
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));

    // Should be at AtFork with NO completed branches (both waited)
    match &snapshot.state {
        WorkflowSnapshotState::InProgress { position, .. } => match position {
            ExecutionPosition::AtFork {
                completed_branches,
                wake_at,
                ..
            } => {
                assert!(
                    completed_branches.is_empty(),
                    "No branches completed, both hit delays"
                );
                // wake_at should be the max of the two delays (200s)
                let min_expected = chrono::Utc::now() + chrono::Duration::seconds(150);
                assert!(
                    *wake_at > min_expected,
                    "wake_at should be ~200s in the future, got {wake_at:?}"
                );
            }
            other => panic!("Expected AtFork, got {other:?}"),
        },
        other => panic!("Expected InProgress, got {other:?}"),
    }
}

#[tokio::test]
async fn test_fork_normal_branch_completes_delayed_branch_parks() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    // branch_a: normal task (completes immediately)
    // branch_b: delay (parks)
    let branch_a = Arc::new(stub_node("branch_a", None));
    let delay = WorkflowContinuation::Delay {
        id: "branch_delay".into(),
        duration: std::time::Duration::from_secs(3600),
        next: None,
    };
    let branch_b = Arc::new(delay);

    let join_task = stub_node("join", None);
    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_task)),
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            match id.as_str() {
                "branch_a" => Ok(encode_u32(decode_u32(&input) * 2)),
                "join" => Ok(input),
                other => Err(format!("Unknown task: {other}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &fork,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));

    // branch_a should have completed, branch_b is waiting
    match &snapshot.state {
        WorkflowSnapshotState::InProgress { position, .. } => match position {
            ExecutionPosition::AtFork {
                completed_branches, ..
            } => {
                assert_eq!(completed_branches.len(), 1);
                assert!(completed_branches.contains_key("branch_a"));
                let result = &completed_branches["branch_a"];
                assert_eq!(decode_u32(&result.output), 20); // 10 * 2
            }
            other => panic!("Expected AtFork, got {other:?}"),
        },
        other => panic!("Expected InProgress, got {other:?}"),
    }
}

// ========================================================================
// Retry tests — sync
// ========================================================================

#[test]
fn test_sync_retry_succeeds_after_failures() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    // max_retries: 2 → backon max_times: 2 → 3 total calls allowed
    let cont = stub_node_with_retry("flaky", fast_retry(2), None);
    let input = encode_u32(10);

    let callback = move |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let attempt = attempts_clone.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            Err("transient error".into())
        } else {
            let val = decode_u32(&input);
            Ok(encode_u32(val + 1))
        }
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
}

#[test]
fn test_sync_retry_exhaustion() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    // max_retries: 1 → backon max_times: 1 → 2 total calls, then error
    let cont = stub_node_with_retry("always_fail", fast_retry(1), None);
    let input = encode_u32(1);

    let callback = move |_id: &str, _input: Bytes| -> Result<Bytes, BoxError> {
        attempts_clone.fetch_add(1, Ordering::SeqCst);
        Err("permanent error".into())
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("permanent error"));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[test]
fn test_sync_retry_no_retry_on_success() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let cont = stub_node_with_retry("ok", fast_retry(2), None);
    let input = encode_u32(5);

    let callback = move |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        attempts_clone.fetch_add(1, Ordering::SeqCst);
        let val = decode_u32(&input);
        Ok(encode_u32(val + 1))
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 6);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn test_sync_retry_in_chain() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let double = stub_node("double", None);
    let flaky = stub_node_with_retry("flaky", fast_retry(2), Some(Box::new(double)));
    let input = encode_u32(10);

    let callback = move |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "flaky" => {
                let attempt = attempts_clone.fetch_add(1, Ordering::SeqCst);
                if attempt < 1 {
                    Err("transient".into())
                } else {
                    Ok(encode_u32(val + 1))
                }
            }
            "double" => Ok(encode_u32(val * 2)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&flaky, input, &callback, &JsonCodec).unwrap();
    // flaky: 10+1=11 (after 1 retry), double: 11*2=22
    assert_eq!(decode_u32(&result), 22);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

// ========================================================================
// Retry tests — async
// ========================================================================

#[tokio::test]
async fn test_async_retry_succeeds_after_failure() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let cont = task_node_with_retry(
        "flaky",
        move |i: u32| {
            let a = attempts_clone.clone();
            async move {
                let attempt = a.fetch_add(1, Ordering::SeqCst);
                if attempt < 1 {
                    Err::<u32, BoxError>("transient".into())
                } else {
                    Ok(i + 1)
                }
            }
        },
        fast_retry(2),
        None,
    );

    let input = encode_u32(10);
    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_async_retry_exhaustion() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let cont = task_node_with_retry(
        "always_fail",
        move |_i: u32| {
            let a = attempts_clone.clone();
            async move {
                a.fetch_add(1, Ordering::SeqCst);
                Err::<u32, BoxError>("permanent".into())
            }
        },
        fast_retry(1),
        None,
    );

    let input = encode_u32(1);
    let result = execute_continuation_async(&cont, input, &JsonCodec).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("permanent"));
    // max_retries: 1 → backon max_times: 1 → 2 total calls
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_async_retry_no_retry_on_success() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let cont = task_node_with_retry(
        "ok",
        move |i: u32| {
            let a = attempts_clone.clone();
            async move {
                a.fetch_add(1, Ordering::SeqCst);
                Ok(i + 1)
            }
        },
        fast_retry(4),
        None,
    );

    let input = encode_u32(42);
    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 43);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_async_retry_with_timeout_triggers_retry() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    // First call: times out. Second call: completes fast.
    let cont = task_node_with_timeout_and_retry(
        "timeout_then_ok",
        move |i: u32| {
            let a = attempts_clone.clone();
            async move {
                let attempt = a.fetch_add(1, Ordering::SeqCst);
                if attempt == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Ok(i + 1)
            }
        },
        std::time::Duration::from_millis(10),
        fast_retry(2),
        None,
    );

    let input = encode_u32(10);
    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_async_retry_in_chain() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let double = task_node("double", |i: u32| async move { Ok(i * 2) }, None);
    let flaky = task_node_with_retry(
        "flaky",
        move |i: u32| {
            let a = attempts_clone.clone();
            async move {
                let attempt = a.fetch_add(1, Ordering::SeqCst);
                if attempt < 1 {
                    Err::<u32, BoxError>("transient".into())
                } else {
                    Ok(i + 1)
                }
            }
        },
        fast_retry(2),
        Some(Box::new(double)),
    );

    let input = encode_u32(5);
    let result = execute_continuation_async(&flaky, input, &JsonCodec)
        .await
        .unwrap();
    // flaky: 5+1=6 (after retry), double: 6*2=12
    assert_eq!(decode_u32(&result), 12);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

// ========================================================================
// Retry tests — checkpointing
// ========================================================================

#[tokio::test]
async fn test_checkpointing_retry_succeeds_after_failure() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "flaky".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let cont = stub_node_with_retry("flaky", fast_retry(2), None);
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let callback = move |_id: &str, input: Bytes| {
        let a = attempts_clone.clone();
        async move {
            let attempt = a.fetch_add(1, Ordering::SeqCst);
            if attempt < 1 {
                Err::<Bytes, BoxError>("transient error".into())
            } else {
                let val: u32 = serde_json::from_slice(&input)?;
                Ok(Bytes::from(serde_json::to_vec(&(val + 1))?))
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    // Task result is cached after success
    assert!(snapshot.get_task_result("flaky").is_some());
    // Retry state is cleared on success
    assert!(snapshot.get_retry_state("flaky").is_none());
}

#[tokio::test]
async fn test_checkpointing_retry_exhaustion() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(1);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "always_fail".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let cont = stub_node_with_retry("always_fail", fast_retry(1), None);
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let callback = move |_id: &str, _input: Bytes| {
        let a = attempts_clone.clone();
        async move {
            a.fetch_add(1, Ordering::SeqCst);
            Err::<Bytes, BoxError>("permanent error".into())
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("permanent error"));
    // Checkpointing: initial + max_retries retries = 1 + 1 = 2 total
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_checkpointing_retry_state_persisted() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "flaky".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    let cont = stub_node_with_retry("flaky", fast_retry(4), None);
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    // Fail 3 times then succeed on 4th
    let callback = move |_id: &str, input: Bytes| {
        let a = attempts_clone.clone();
        async move {
            let attempt = a.fetch_add(1, Ordering::SeqCst);
            if attempt < 3 {
                Err::<Bytes, BoxError>(format!("error #{attempt}").into())
            } else {
                let val: u32 = serde_json::from_slice(&input)?;
                Ok(Bytes::from(serde_json::to_vec(&(val + 1))?))
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 4);

    // Retry state should be cleared after success
    assert!(snapshot.get_retry_state("flaky").is_none());

    // Snapshot was saved to backend during retries — verify it has the task result
    let persisted = backend.load_snapshot("inst-1").await.unwrap();
    assert!(persisted.get_task_result("flaky").is_some());
}

#[tokio::test]
async fn test_checkpointing_retry_with_timeout() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    snapshot.update_position(ExecutionPosition::AtTask {
        task_id: "slow_then_fast".into(),
    });
    backend.save_snapshot(&snapshot).await.unwrap();

    // Timeout of 10ms; first attempt sleeps 200ms (triggers timeout), second is instant
    let cont = stub_node_with_timeout_and_retry(
        "slow_then_fast",
        std::time::Duration::from_millis(10),
        fast_retry(2),
        None,
    );

    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let callback = move |_id: &str, input: Bytes| {
        let a = attempts_clone.clone();
        async move {
            let attempt = a.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Ok(input)
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 10);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_checkpointing_retry_in_chain() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let double = stub_node("double", None);
    let flaky = stub_node_with_retry("flaky", fast_retry(2), Some(Box::new(double)));

    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let callback = move |id: &str, input: Bytes| {
        let id = id.to_string();
        let a = attempts_clone.clone();
        async move {
            let val: u32 = serde_json::from_slice(&input)?;
            match id.as_str() {
                "flaky" => {
                    let attempt = a.fetch_add(1, Ordering::SeqCst);
                    if attempt < 1 {
                        Err::<Bytes, BoxError>("transient".into())
                    } else {
                        Ok(Bytes::from(serde_json::to_vec(&(val + 1))?))
                    }
                }
                "double" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &flaky,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // flaky: 5+1=6 (after retry), double: 6*2=12
    assert_eq!(decode_u32(&result), 12);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    // Both tasks should be cached
    assert!(snapshot.get_task_result("flaky").is_some());
    assert!(snapshot.get_task_result("double").is_some());
}

// ========================================================================
// Timeout + chain tests
// ========================================================================

#[tokio::test]
async fn test_async_timeout_mid_chain_fails() {
    // First task succeeds, second task times out → chain fails
    let slow_task = WorkflowContinuation::Task {
        id: "slow".to_string(),
        func: Some(to_core_task(
            "slow",
            |i: u32| async move {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                Ok(i * 2)
            },
            codec(),
        )),
        timeout: Some(std::time::Duration::from_millis(5)),
        retry_policy: None,
        version: None,
        next: None,
    };
    let fast_task = task_node(
        "fast",
        |i: u32| async move { Ok(i + 1) },
        Some(Box::new(slow_task)),
    );

    let input = encode_u32(10);
    let result = execute_continuation_async(&fast_task, input, &JsonCodec).await;
    let err = result.unwrap_err();
    assert!(err.to_string().contains("timed out"));
    assert!(err.to_string().contains("slow"));
}

#[tokio::test]
async fn test_async_timeout_passes_in_chain() {
    // Both tasks have timeouts but complete within them → chain succeeds
    let second = WorkflowContinuation::Task {
        id: "second".to_string(),
        func: Some(to_core_task(
            "second",
            |i: u32| async move { Ok(i * 2) },
            codec(),
        )),
        timeout: Some(std::time::Duration::from_secs(5)),
        retry_policy: None,
        version: None,
        next: None,
    };
    let first = WorkflowContinuation::Task {
        id: "first".to_string(),
        func: Some(to_core_task(
            "first",
            |i: u32| async move { Ok(i + 1) },
            codec(),
        )),
        timeout: Some(std::time::Duration::from_secs(5)),
        retry_policy: None,
        version: None,
        next: Some(Box::new(second)),
    };

    let input = encode_u32(10);
    let result = execute_continuation_async(&first, input, &JsonCodec)
        .await
        .unwrap();
    // first: 10+1=11, second: 11*2=22
    assert_eq!(decode_u32(&result), 22);
}

#[tokio::test]
async fn test_checkpointing_timeout_mid_chain() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    // fast_task → slow_task (times out)
    let slow_task = WorkflowContinuation::Task {
        id: "slow".to_string(),
        func: None,
        timeout: Some(std::time::Duration::from_millis(10)),
        retry_policy: None,
        version: None,
        next: None,
    };
    let fast_task = WorkflowContinuation::Task {
        id: "fast".to_string(),
        func: None,
        timeout: None,
        retry_policy: None,
        version: None,
        next: Some(Box::new(slow_task)),
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            match id.as_str() {
                "fast" => Ok(input),
                "slow" => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    Ok(input)
                }
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &fast_task,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    let err = result.unwrap_err();
    assert!(err.to_string().contains("timed out"));
    assert!(err.to_string().contains("slow"));
    // First task should still be cached
    assert!(snapshot.get_task_result("fast").is_some());
}

// ========================================================================
// Delay edge cases
// ========================================================================

#[tokio::test]
async fn test_checkpointing_delay_terminal_parks() {
    // A delay with no next (terminal) should park and return Waiting
    let backend = InMemoryBackend::new();
    let input = encode_u32(42);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let delay = WorkflowContinuation::Delay {
        id: "final_wait".into(),
        duration: std::time::Duration::from_secs(3600),
        next: None,
    };

    let callback =
        |_id: &str, _input: Bytes| async { Err::<Bytes, BoxError>("Should not be called".into()) };

    let result = execute_continuation_with_checkpointing(
        &delay,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    assert!(result.is_err());
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));

    // next_task_id should be None for terminal delay
    match &snapshot.state {
        WorkflowSnapshotState::InProgress { position, .. } => match position {
            ExecutionPosition::AtDelay {
                delay_id,
                next_task_id,
                ..
            } => {
                assert_eq!(delay_id, "final_wait");
                assert!(next_task_id.is_none());
            }
            other => panic!("Expected AtDelay, got {other:?}"),
        },
        other => panic!("Expected InProgress, got {other:?}"),
    }

    // Pass-through value should be stored
    let stored = snapshot.get_task_result("final_wait").unwrap();
    assert_eq!(decode_u32(&stored.output), 42);
}

#[tokio::test]
async fn test_checkpointing_delay_after_task_chain() {
    // task → delay → task: first task completes, delay parks, resume completes chain
    let backend = InMemoryBackend::new();
    let input = encode_u32(10);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let process = stub_node("process", None);
    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_millis(1), // very short for resume test
        next: Some(Box::new(process)),
    };
    let prepare = stub_node("prepare", Some(Box::new(delay)));

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val: u32 = serde_json::from_slice(&input)?;
            match id.as_str() {
                "prepare" => Ok(Bytes::from(serde_json::to_vec(&(val + 1))?)),
                "process" => Ok(Bytes::from(serde_json::to_vec(&(val * 2))?)),
                _ => Err(format!("Unknown: {id}").into()),
            }
        }
    };

    // First run: prepare completes, delay parks
    let result = execute_continuation_with_checkpointing(
        &prepare,
        input.clone(),
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;
    assert!(matches!(
        result.unwrap_err(),
        RuntimeError::Workflow(WorkflowError::Waiting { .. })
    ));
    assert!(snapshot.get_task_result("prepare").is_some());

    // Wait for delay to expire
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;

    // Reload snapshot and re-execute — prepare is cached, delay skipped, process runs
    snapshot = backend.load_snapshot("inst-1").await.unwrap();
    let result = execute_continuation_with_checkpointing(
        &prepare,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // prepare: 10+1=11, delay (passthrough 11), process: 11*2=22
    assert_eq!(decode_u32(&result), 22);
}

#[test]
fn test_sync_delay_multiple_in_chain() {
    // task → delay → delay → task: two consecutive delays pass input through
    let final_task = stub_node("final", None);
    let delay2 = WorkflowContinuation::Delay {
        id: "wait2".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(final_task)),
    };
    let delay1 = WorkflowContinuation::Delay {
        id: "wait1".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(delay2)),
    };
    let first = stub_node("first", Some(Box::new(delay1)));

    let input = encode_u32(7);
    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "first" => Ok(encode_u32(val + 3)),
            "final" => Ok(encode_u32(val * 10)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&first, input, &callback, &JsonCodec).unwrap();
    // first: 7+3=10, delay1: pass 10, delay2: pass 10, final: 10*10=100
    assert_eq!(decode_u32(&result), 100);
}

#[tokio::test]
async fn test_async_delay_multiple_in_chain() {
    let final_task = task_node("final", |i: u32| async move { Ok(i * 10) }, None);
    let delay2 = WorkflowContinuation::Delay {
        id: "wait2".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(final_task)),
    };
    let delay1 = WorkflowContinuation::Delay {
        id: "wait1".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(delay2)),
    };
    let first = task_node(
        "first",
        |i: u32| async move { Ok(i + 3) },
        Some(Box::new(delay1)),
    );

    let input = encode_u32(7);
    let result = execute_continuation_async(&first, input, &JsonCodec)
        .await
        .unwrap();
    // first: 7+3=10, delay1: pass 10, delay2: pass 10, final: 10*10=100
    assert_eq!(decode_u32(&result), 100);
}

// ========================================================================
// Retry + delay combinations
// ========================================================================

#[test]
fn test_sync_retry_after_delay() {
    // delay → retry task: delay passes through, retry task eventually succeeds
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let retry_task = stub_node_with_retry("retry_task", fast_retry(2), None);
    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(retry_task)),
    };

    let input = encode_u32(10);
    let callback = move |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        match id {
            "retry_task" => {
                let attempt = attempts_clone.fetch_add(1, Ordering::SeqCst);
                if attempt < 1 {
                    Err("transient".into())
                } else {
                    let val = decode_u32(&input);
                    Ok(encode_u32(val + 1))
                }
            }
            _ => Err(format!("Unknown: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&delay, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn test_async_retry_after_delay() {
    let attempts = Arc::new(AtomicU32::new(0));
    let attempts_clone = attempts.clone();

    let retry_task = task_node_with_retry(
        "retry_task",
        move |i: u32| {
            let a = attempts_clone.clone();
            async move {
                let attempt = a.fetch_add(1, Ordering::SeqCst);
                if attempt < 1 {
                    Err::<u32, BoxError>("transient".into())
                } else {
                    Ok(i + 1)
                }
            }
        },
        fast_retry(2),
        None,
    );

    let delay = WorkflowContinuation::Delay {
        id: "wait".into(),
        duration: std::time::Duration::from_millis(1),
        next: Some(Box::new(retry_task)),
    };

    let input = encode_u32(10);
    let result = execute_continuation_async(&delay, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 11);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

// ========================================================================
// Proptests
// ========================================================================

mod proptests {
    use super::*;
    use proptest::prelude::*;

    // Property 1: Roundtrip identity — serialize then deserialize recovers the same entries.
    proptest! {
        #[test]
        fn serialize_deserialize_roundtrip(
            entries in proptest::collection::vec(
                (
                    "[a-z]{0,32}",
                    proptest::collection::vec(any::<u8>(), 0..64),
                ),
                0..8,
            )
        ) {
            let typed: Vec<(String, Bytes)> = entries
                .into_iter()
                .map(|(n, d)| (n, Bytes::from(d)))
                .collect();

            let serialized = serialize_branch_results(&typed, &JsonCodec).unwrap();
            let deserialized: NamedBranchResults = serde_json::from_slice(&serialized).unwrap();

            prop_assert_eq!(deserialized.as_slice(), typed.as_slice());
        }
    }

    // Property 11: `get_resume_input` always errors for non-InProgress states.
    proptest! {
        #[test]
        fn non_in_progress_always_errors(
            variant in 0..3u8,
            error_msg in "[a-zA-Z0-9 ]{0,32}",
            reason in prop::option::of("[a-zA-Z0-9 ]{0,32}"),
            cancelled_by in prop::option::of("[a-zA-Z0-9 ]{0,32}"),
            output_data in proptest::collection::vec(any::<u8>(), 0..32),
        ) {
            let mut snapshot = WorkflowSnapshot::new("inst".into(), "hash".into());
            match variant {
                0 => snapshot.mark_completed(Bytes::from(output_data)),
                1 => snapshot.mark_failed(error_msg),
                _ => snapshot.mark_cancelled(reason, cancelled_by, None),
            }

            let result = get_resume_input(&snapshot);
            prop_assert!(result.is_err(), "Expected Err for non-InProgress state");
        }
    }

    // Property 12: InProgress with no completed tasks returns the initial input.
    proptest! {
        #[test]
        fn in_progress_empty_tasks_returns_initial_input(
            input_data in proptest::collection::vec(any::<u8>(), 1..64),
        ) {
            let initial = Bytes::from(input_data);
            let snapshot = WorkflowSnapshot::with_initial_input(
                "inst".into(),
                "hash".into(),
                initial.clone(),
            );

            let result = get_resume_input(&snapshot).unwrap();
            prop_assert_eq!(result, initial);
        }
    }
}

// ── Branch tests ──────────────────────────────────────────────────────

fn branch_node(
    id: &str,
    key: &'static str,
    branches: std::collections::HashMap<String, Box<WorkflowContinuation>>,
    default: Option<Box<WorkflowContinuation>>,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation {
    let c = codec();
    let key_task = to_core_task(
        id,
        move |_input: serde_json::Value| async move { Ok(key.to_string()) },
        c,
    );
    WorkflowContinuation::Branch {
        id: id.to_string(),
        key_fn: Some(key_task),
        branches,
        default,
        next,
    }
}

#[test]
fn test_sync_branch_selects_correct_branch() {
    let billing = stub_node("handle_billing", None);
    let tech = stub_node("handle_tech", None);

    let mut branches = std::collections::HashMap::new();
    branches.insert("billing".to_string(), Box::new(billing));
    branches.insert("tech".to_string(), Box::new(tech));

    let branch = branch_node("route", "billing", branches, None, None);

    let input = encode_u32(10);
    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "route::key_fn" => Ok(Bytes::from(serde_json::to_vec("billing").unwrap())),
            "handle_billing" => Ok(encode_u32(val * 100)),
            "handle_tech" => Ok(encode_u32(val + 1)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&branch, input, &callback, &JsonCodec).unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(envelope["branch"], "billing");
    assert_eq!(envelope["result"], 1000); // 10 * 100
}

#[test]
fn test_sync_branch_uses_default() {
    let billing = stub_node("handle_billing", None);
    let fallback = stub_node("handle_fallback", None);

    let mut branches = std::collections::HashMap::new();
    branches.insert("billing".to_string(), Box::new(billing));

    let branch = branch_node(
        "route",
        "unknown_key",
        branches,
        Some(Box::new(fallback)),
        None,
    );

    let input = encode_u32(5);
    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "route::key_fn" => Ok(Bytes::from(serde_json::to_vec("unknown_key").unwrap())),
            "handle_billing" => Ok(encode_u32(val * 100)),
            "handle_fallback" => Ok(encode_u32(val + 999)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&branch, input, &callback, &JsonCodec).unwrap();
    let envelope: serde_json::Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(envelope["branch"], "unknown_key");
    assert_eq!(envelope["result"], 1004); // 5 + 999
}

#[test]
fn test_sync_branch_key_not_found() {
    let billing = stub_node("handle_billing", None);

    let mut branches = std::collections::HashMap::new();
    branches.insert("billing".to_string(), Box::new(billing));

    let branch = branch_node("route", "nonexistent", branches, None, None);

    let input = encode_u32(5);
    let callback = |id: &str, _input: Bytes| -> Result<Bytes, BoxError> {
        match id {
            "route::key_fn" => Ok(Bytes::from(serde_json::to_vec("nonexistent").unwrap())),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let err = execute_continuation_sync(&branch, input, &callback, &JsonCodec).unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("no branch matches key 'nonexistent'"),
        "Error was: {err_str}"
    );
}

#[test]
fn test_sync_branch_then_next() {
    let billing = stub_node("handle_billing", None);

    let mut branches = std::collections::HashMap::new();
    branches.insert("billing".to_string(), Box::new(billing));

    let next = stub_node("finalize", None);
    let branch = branch_node("route", "billing", branches, None, Some(Box::new(next)));

    let input = encode_u32(10);
    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        match id {
            "route::key_fn" => Ok(Bytes::from(serde_json::to_vec("billing").unwrap())),
            "handle_billing" => {
                let val = decode_u32(&input);
                Ok(encode_u32(val * 2))
            }
            "finalize" => {
                // Receives BranchEnvelope JSON
                let envelope: serde_json::Value = serde_json::from_slice(&input).unwrap();
                assert_eq!(envelope["branch"], "billing");
                #[allow(clippy::cast_possible_truncation)]
                let inner = envelope["result"].as_u64().unwrap() as u32;
                Ok(encode_u32(inner + 1))
            }
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&branch, input, &callback, &JsonCodec).unwrap();
    // handle_billing: 10*2=20, finalize: 20+1=21
    assert_eq!(decode_u32(&result), 21);
}

// Async and checkpointing branch tests are covered by the distributed runner tests
// in sayiir-runtime/src/runner/distributed.rs (test_route_*).

// ========================================================================
// Loop helpers
// ========================================================================

fn loop_body_task(
    id: &str,
    f: impl Fn(
        u32,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<sayiir_core::LoopResult<u32>, BoxError>> + Send,
        >,
    > + Send
    + Sync
    + 'static,
) -> WorkflowContinuation {
    let c = codec();
    WorkflowContinuation::Task {
        id: id.to_string(),
        func: Some(to_core_task(id, f, c)),
        timeout: None,
        retry_policy: None,
        version: None,
        next: None,
    }
}

// ========================================================================
// Loop tests — checkpointing (resume from checkpoint)
// ========================================================================

#[tokio::test]
async fn test_checkpointing_loop_basic() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(3);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let body = stub_node("countdown", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };

    let callback = |_id: &str, input: Bytes| async move {
        let n = decode_u32(&input);
        if n == 0 {
            Ok(encode_loop_done(0))
        } else {
            Ok(encode_loop_again(n - 1))
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 0);
    // Loop iteration counter should be cleared after completion
    assert_eq!(snapshot.loop_iteration("loop_0"), 0);
}

#[tokio::test]
async fn test_checkpointing_loop_resumes_from_iteration() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());

    // Simulate: 2 iterations already completed. Next input should be 3 (5→4→3).
    snapshot.set_loop_iteration("loop_0", 2);
    backend.save_snapshot(&snapshot).await.unwrap();

    let call_count = Arc::new(AtomicU32::new(0));
    let cc = call_count.clone();

    let body = stub_node("countdown", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };

    let callback = move |_id: &str, input: Bytes| {
        let cc = cc.clone();
        async move {
            cc.fetch_add(1, Ordering::SeqCst);
            let n = decode_u32(&input);
            if n == 0 {
                Ok(encode_loop_done(0))
            } else {
                Ok(encode_loop_again(n - 1))
            }
        }
    };

    // Resume with input 3 (what iteration 2 would have produced)
    let resume_input = encode_u32(3);

    let result = execute_continuation_with_checkpointing(
        &cont,
        resume_input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 0);
    // Should have run 4 body executions (3→2→1→0 → Done)
    assert_eq!(call_count.load(Ordering::SeqCst), 4);
    // Loop iteration counter should be cleared after completion
    assert_eq!(snapshot.loop_iteration("loop_0"), 0);
}

// ========================================================================
// Loop tests — sync
// ========================================================================

fn encode_loop_again(val: u32) -> Bytes {
    sayiir_core::codec::encode_loop_envelope(
        sayiir_core::codec::LoopDecision::Again,
        &serde_json::to_vec(&val).unwrap(),
    )
}

fn encode_loop_done(val: u32) -> Bytes {
    sayiir_core::codec::encode_loop_envelope(
        sayiir_core::codec::LoopDecision::Done,
        &serde_json::to_vec(&val).unwrap(),
    )
}

fn loop_node(
    body_id: &str,
    max_iterations: u32,
    on_max: sayiir_core::workflow::MaxIterationsPolicy,
    next: Option<Box<WorkflowContinuation>>,
) -> WorkflowContinuation {
    WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(stub_node(body_id, None)),
        max_iterations,
        on_max,
        next: next.map(Into::into),
    }
}

#[test]
fn test_sync_loop_done_immediately() {
    use sayiir_core::workflow::MaxIterationsPolicy;

    let cont = loop_node("body", 10, MaxIterationsPolicy::Fail, None);
    let input = encode_u32(42);

    let callback = |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        Ok(encode_loop_done(val * 2))
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 84);
}

#[test]
fn test_sync_loop_three_iterations() {
    use sayiir_core::workflow::MaxIterationsPolicy;

    let cont = loop_node("countdown", 10, MaxIterationsPolicy::Fail, None);
    let input = encode_u32(3);

    let callback = |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let n = decode_u32(&input);
        if n <= 0 {
            Ok(encode_loop_done(0))
        } else {
            Ok(encode_loop_again(n - 1))
        }
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    assert_eq!(decode_u32(&result), 0);
}

#[test]
fn test_sync_loop_max_iterations_fail() {
    use sayiir_core::workflow::MaxIterationsPolicy;

    let cont = loop_node("always_again", 3, MaxIterationsPolicy::Fail, None);
    let input = encode_u32(0);

    let callback = |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let n = decode_u32(&input);
        Ok(encode_loop_again(n + 1))
    };

    let err = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap_err();
    assert!(
        err.to_string().contains("max"),
        "expected MaxIterationsExceeded, got: {err}"
    );
}

#[test]
fn test_sync_loop_max_iterations_exit_with_last() {
    use sayiir_core::workflow::MaxIterationsPolicy;

    let cont = loop_node("always_again", 3, MaxIterationsPolicy::ExitWithLast, None);
    let input = encode_u32(0);

    let callback = |_id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let n = decode_u32(&input);
        Ok(encode_loop_again(n + 1))
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    // 0 → again(1) → again(2) → again(3) → max reached, exit with 3
    assert_eq!(decode_u32(&result), 3);
}

#[test]
fn test_sync_loop_in_chain() {
    use sayiir_core::workflow::MaxIterationsPolicy;

    let double = stub_node("double", None);
    let cont = loop_node(
        "countdown",
        10,
        MaxIterationsPolicy::Fail,
        Some(Box::new(double)),
    );
    let input = encode_u32(3);

    let callback = |id: &str, input: Bytes| -> Result<Bytes, BoxError> {
        let val = decode_u32(&input);
        match id {
            "countdown" => {
                if val <= 0 {
                    Ok(encode_loop_done(0))
                } else {
                    Ok(encode_loop_again(val - 1))
                }
            }
            "double" => Ok(encode_u32(val * 2)),
            _ => Err(format!("Unknown task: {id}").into()),
        }
    };

    let result = execute_continuation_sync(&cont, input, &callback, &JsonCodec).unwrap();
    // 3 → 2 → 1 → 0 → done(0) → double → 0
    assert_eq!(decode_u32(&result), 0);
}

// ========================================================================
// Loop tests — async
// ========================================================================

#[tokio::test]
async fn test_async_loop_done_immediately() {
    use sayiir_core::LoopResult;

    let body = loop_body_task("body", |val| {
        Box::pin(async move { Ok(LoopResult::Done(val * 2)) })
    });
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };
    let input = encode_u32(42);

    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 84);
}

#[tokio::test]
async fn test_async_loop_max_iterations_fail() {
    use sayiir_core::LoopResult;

    let body = loop_body_task("always_again", |val| {
        Box::pin(async move { Ok(LoopResult::Again(val + 1)) })
    });
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 3,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };
    let input = encode_u32(0);

    let err = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("max"),
        "expected MaxIterationsExceeded, got: {err}"
    );
}

// ========================================================================
// Loop inside fork branch — async
// ========================================================================

#[tokio::test]
async fn test_async_loop_inside_fork_branch() {
    use sayiir_core::LoopResult;
    use sayiir_core::task::{BranchOutputs, to_heterogeneous_join_task};

    // Branch A: a loop that counts down from input to 0
    let loop_body_a = loop_body_task("countdown", |n| {
        Box::pin(async move {
            if n == 0 {
                Ok(LoopResult::Done(100u32))
            } else {
                Ok(LoopResult::Again(n - 1))
            }
        })
    });
    let branch_a = Arc::new(WorkflowContinuation::Loop {
        id: "loop_a".into(),
        body: Box::new(loop_body_a),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    });

    // Branch B: simple doubler
    let branch_b = Arc::new(task_node("double", |x: u32| async move { Ok(x * 2) }, None));

    // Join sums both branch results
    let join_fn = to_heterogeneous_join_task(
        "join",
        |outputs: BranchOutputs<JsonCodec>| async move {
            let a: u32 = outputs.get_by_id("loop_a")?;
            let b: u32 = outputs.get_by_id("double")?;
            Ok(a + b)
        },
        codec(),
    );
    let join_cont = WorkflowContinuation::Task {
        id: "join".to_string(),
        func: Some(join_fn),
        timeout: None,
        retry_policy: None,
        version: None,
        next: None,
    };

    let fork = WorkflowContinuation::Fork {
        id: "fork".into(),
        branches: vec![branch_a, branch_b].into_boxed_slice(),
        join: Some(Box::new(join_cont)),
    };
    let input = encode_u32(3);

    let result = execute_continuation_async(&fork, input, &JsonCodec)
        .await
        .unwrap();
    // loop_a: countdown 3→2→1→0 → Done(100), double: 3*2=6, join: 100+6=106
    assert_eq!(decode_u32(&result), 106);
}

// ========================================================================
// Loop tests — async (additional coverage)
// ========================================================================

#[tokio::test]
async fn test_async_loop_three_iterations() {
    use sayiir_core::LoopResult;

    let body = loop_body_task("countdown", |n| {
        Box::pin(async move {
            if n == 0 {
                Ok(LoopResult::Done(n))
            } else {
                Ok(LoopResult::Again(n - 1))
            }
        })
    });
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };
    let input = encode_u32(3);

    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    assert_eq!(decode_u32(&result), 0);
}

#[tokio::test]
async fn test_async_loop_exit_with_last() {
    use sayiir_core::LoopResult;

    let body = loop_body_task("always_again", |val| {
        Box::pin(async move { Ok(LoopResult::Again(val + 1)) })
    });
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 3,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::ExitWithLast,
        next: None,
    };
    let input = encode_u32(0);

    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    // 0 → again(1) → again(2) → again(3) → max reached, exit with 3
    assert_eq!(decode_u32(&result), 3);
}

#[tokio::test]
async fn test_async_loop_in_chain() {
    use sayiir_core::LoopResult;

    let double = task_node("double", |x: u32| async move { Ok(x * 2) }, None);
    let body = loop_body_task("countdown", |n| {
        Box::pin(async move {
            if n == 0 {
                Ok(LoopResult::Done(10u32))
            } else {
                Ok(LoopResult::Again(n - 1))
            }
        })
    });
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: Some(Box::new(double).into()),
    };
    let input = encode_u32(3);

    let result = execute_continuation_async(&cont, input, &JsonCodec)
        .await
        .unwrap();
    // 3→2→1→0 → Done(10) → double → 20
    assert_eq!(decode_u32(&result), 20);
}

// ========================================================================
// Loop tests — checkpointing (additional coverage)
// ========================================================================

#[tokio::test]
async fn test_checkpointing_loop_caches_result_on_exit() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(2);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let body = stub_node("countdown", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };

    let callback = |_id: &str, input: Bytes| async move {
        let n = decode_u32(&input);
        if n == 0 {
            Ok(encode_loop_done(0))
        } else {
            Ok(encode_loop_again(n - 1))
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 0);

    // The loop node itself should have a cached result in the snapshot.
    let cached = snapshot.get_task_result("loop_0");
    assert!(cached.is_some(), "loop node should be cached after exit");
    assert_eq!(decode_u32(&cached.unwrap().output), 0);
}

#[tokio::test]
async fn test_checkpointing_loop_short_circuits_when_cached() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(99);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());

    // Pre-cache a result for the loop node.
    let cached_output = encode_u32(42);
    snapshot.mark_task_completed("loop_0".into(), cached_output.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let body = stub_node("body", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };

    let call_count = Arc::new(AtomicU32::new(0));
    let cc = call_count.clone();
    let callback = move |_id: &str, _input: Bytes| {
        let cc = cc.clone();
        async move {
            cc.fetch_add(1, Ordering::SeqCst);
            Ok(encode_loop_done(0))
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // Should return the cached result without executing any body tasks.
    assert_eq!(decode_u32(&result), 42);
    assert_eq!(call_count.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn test_checkpointing_loop_exit_with_last() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(0);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let body = stub_node("always_again", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 3,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::ExitWithLast,
        next: None,
    };

    let callback = |_id: &str, input: Bytes| async move {
        let n = decode_u32(&input);
        Ok(encode_loop_again(n + 1))
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // 0 → again(1) → again(2) → again(3) → max reached, exit with 3
    assert_eq!(decode_u32(&result), 3);

    // Should be cached under the loop node.
    let cached = snapshot.get_task_result("loop_0");
    assert!(
        cached.is_some(),
        "loop node should be cached after exit_with_last"
    );
    assert_eq!(decode_u32(&cached.unwrap().output), 3);

    // Iteration counter should be cleared.
    assert_eq!(snapshot.loop_iteration("loop_0"), 0);
}

#[tokio::test]
async fn test_checkpointing_loop_in_chain() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(2);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let double = stub_node("double", None);
    let body = stub_node("countdown", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: Some(Box::new(double).into()),
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val = decode_u32(&input);
            match id.as_str() {
                "countdown" => {
                    if val == 0 {
                        Ok(encode_loop_done(10))
                    } else {
                        Ok(encode_loop_again(val - 1))
                    }
                }
                "double" => Ok(encode_u32(val * 2)),
                _ => Err(format!("Unknown task: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    // 2→1→0 → Done(10) → double → 20
    assert_eq!(decode_u32(&result), 20);

    // Loop node should be cached (even though there's a next step).
    assert!(snapshot.get_task_result("loop_0").is_some());
}

#[tokio::test]
async fn test_checkpointing_loop_inside_fork_branch() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(2);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let body = stub_node("countdown", None);
    let loop_branch = Arc::new(WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    });

    let passthrough = Arc::new(stub_node("passthrough", None));

    let fork = WorkflowContinuation::Fork {
        id: "fork_0".into(),
        branches: vec![loop_branch, passthrough].into_boxed_slice(),
        join: None,
    };

    let callback = |id: &str, input: Bytes| {
        let id = id.to_string();
        async move {
            let val = decode_u32(&input);
            match id.as_str() {
                "countdown" => {
                    if val == 0 {
                        Ok(encode_loop_done(0))
                    } else {
                        Ok(encode_loop_again(val - 1))
                    }
                }
                "passthrough" => Ok(encode_u32(val)),
                _ => Err(format!("Unknown task: {id}").into()),
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &fork,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await;

    // Fork with no join returns the named results envelope.
    // Just verify it doesn't error — the loop inside a branch executes correctly.
    assert!(
        result.is_ok(),
        "loop inside fork branch should succeed: {:?}",
        result.err()
    );

    // The loop node should be cached in the snapshot.
    assert!(
        snapshot.get_task_result("loop_0").is_some(),
        "loop inside fork branch should be cached"
    );
}

#[tokio::test]
async fn test_checkpointing_loop_iteration_counter_persisted() {
    let backend = InMemoryBackend::new();
    let input = encode_u32(5);

    let mut snapshot =
        WorkflowSnapshot::with_initial_input("inst-1".into(), "hash-1".into(), input.clone());
    backend.save_snapshot(&snapshot).await.unwrap();

    let body = stub_node("countdown", None);
    let cont = WorkflowContinuation::Loop {
        id: "loop_0".into(),
        body: Box::new(body),
        max_iterations: 10,
        on_max: sayiir_core::workflow::MaxIterationsPolicy::Fail,
        next: None,
    };

    let call_count = Arc::new(AtomicU32::new(0));
    let cc = call_count.clone();

    let callback = move |_id: &str, input: Bytes| {
        let cc = cc.clone();
        async move {
            let n = decode_u32(&input);
            cc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(encode_loop_done(0))
            } else {
                Ok(encode_loop_again(n - 1))
            }
        }
    };

    let result = execute_continuation_with_checkpointing(
        &cont,
        input,
        &mut snapshot,
        &backend,
        &callback,
        &JsonCodec,
    )
    .await
    .unwrap();

    assert_eq!(decode_u32(&result), 0);
    assert_eq!(call_count.load(Ordering::SeqCst), 6); // 5→4→3→2→1→0

    // After completion, the backend's snapshot should also have the cached result.
    let persisted = backend.load_snapshot("inst-1").await.unwrap();
    assert!(
        persisted.get_task_result("loop_0").is_some(),
        "loop result should be persisted to backend"
    );
    assert_eq!(persisted.loop_iteration("loop_0"), 0);
}
