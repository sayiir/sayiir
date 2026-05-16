#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Tests for the `workflow! { deps: ... }` field and the underlying
//! `Deps` / `from_deps` / `verify_deps` machinery.

use sayiir_core::deps::Deps;
use sayiir_core::error::{BoxError, BuildError, BuildErrors};
use sayiir_core::registry::TaskRegistry;
use sayiir_core::task::CoreTask;
use sayiir_core::workflow::SerializableWorkflow;
use sayiir_macros::{task, workflow};
use sayiir_runtime::InProcessRunner;
use sayiir_runtime::WorkflowRunner;
use sayiir_runtime::serialization::JsonCodec;
use std::sync::Arc;

type BuildResult = Result<SerializableWorkflow<JsonCodec, u32, ()>, BuildErrors>;

fn expect_err(wf: BuildResult, ctx: &str) -> BuildErrors {
    match wf {
        Ok(_) => panic!("expected build to fail: {ctx}"),
        Err(e) => e,
    }
}

fn expect_ok(wf: BuildResult, ctx: &str) -> SerializableWorkflow<JsonCodec, u32, ()> {
    match wf {
        Ok(w) => w,
        Err(e) => panic!("{ctx}: {e}"),
    }
}

// ─── Fixtures ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct HttpClient {
    base_url: String,
}

#[derive(Debug, Clone)]
struct Counter {
    factor: u32,
}

#[task(id = "deps_fetch")]
async fn deps_fetch(input: u32, #[inject] client: Arc<HttpClient>) -> Result<u32, BoxError> {
    Ok(input + client.base_url.len() as u32)
}

#[task(id = "deps_multiply")]
async fn deps_multiply(input: u32, #[inject] counter: Arc<Counter>) -> Result<u32, BoxError> {
    Ok(input * counter.factor)
}

#[task(id = "deps_plain")]
async fn deps_plain(input: u32) -> Result<u32, BoxError> {
    Ok(input + 1)
}

// ─── 1. from_deps / verify_deps are generated for every task ────────────────

#[test]
fn no_inject_task_from_deps_returns_default() {
    let deps = Deps::new();
    // Compiles and runs even when Deps is empty.
    let _t = DepsPlainTask::from_deps(&deps);
    assert!(DepsPlainTask::verify_deps(&deps).is_empty());
}

#[test]
fn inject_task_from_deps_resolves_arc() {
    let client = Arc::new(HttpClient {
        base_url: "https://api.example.com".to_string(),
    });
    let deps = Deps::builder().insert(client.clone()).build();

    let task = DepsFetchTask::from_deps(&deps);
    let fut = task.run(10u32);
    let result = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fut)
        .unwrap();
    assert_eq!(result, 10 + client.base_url.len() as u32);
}

#[test]
fn verify_deps_reports_missing_arc() {
    let deps = Deps::new();
    let missing = DepsFetchTask::verify_deps(&deps);
    assert_eq!(missing.len(), 1);
    assert!(missing[0].type_name.contains("HttpClient"));
}

#[test]
fn verify_deps_passes_when_all_present() {
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "x".into(),
        }))
        .build();
    assert!(DepsFetchTask::verify_deps(&deps).is_empty());
}

// ─── 2. workflow! macro with `deps:` field ───────────────────────────────────

#[test]
fn workflow_macro_builds_with_deps() {
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "https://api".into(),
        }))
        .insert(Arc::new(Counter { factor: 5 }))
        .build();

    expect_ok(
        workflow! {
            name: "checkout",
            codec: JsonCodec,
            deps: &deps,
            steps: [deps_fetch, deps_multiply, deps_plain]
        },
        "checkout build",
    );
}

#[test]
fn workflow_macro_surfaces_missing_dep_as_build_error() {
    let deps = Deps::new(); // empty — both tasks have unmet inject deps.

    let errs = expect_err(
        workflow! {
            name: "checkout-broken",
            codec: JsonCodec,
            deps: &deps,
            steps: [deps_fetch, deps_multiply]
        },
        "both tasks missing deps",
    );
    let collected: Vec<_> = errs.into_iter().collect();
    assert_eq!(collected.len(), 2);

    let task_ids: Vec<&str> = collected
        .iter()
        .filter_map(|e| match e {
            BuildError::MissingDep { task_id, .. } => Some(*task_id),
            _ => None,
        })
        .collect();
    assert!(task_ids.contains(&"deps_fetch"));
    assert!(task_ids.contains(&"deps_multiply"));

    // Each error should name the missing type.
    for e in &collected {
        let s = format!("{e}");
        assert!(s.contains("HttpClient") || s.contains("Counter"));
    }
}

#[test]
fn workflow_macro_partial_missing_dep() {
    // HttpClient present, Counter missing.
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "x".into(),
        }))
        .build();

    let errs = expect_err(
        workflow! {
            name: "checkout-partial",
            codec: JsonCodec,
            deps: &deps,
            steps: [deps_fetch, deps_multiply]
        },
        "Counter missing",
    );
    let collected: Vec<_> = errs.into_iter().collect();
    assert_eq!(collected.len(), 1);
    match &collected[0] {
        BuildError::MissingDep { task_id, type_name } => {
            assert_eq!(*task_id, "deps_multiply");
            assert!(type_name.contains("Counter"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn workflow_macro_mixing_inject_and_plain_tasks() {
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "https://api".into(),
        }))
        .insert(Arc::new(Counter { factor: 3 }))
        .build();

    let wf: BuildResult = workflow! {
        name: "mixed",
        codec: JsonCodec,
        deps: &deps,
        steps: [deps_plain, deps_fetch, deps_multiply]
    };

    assert!(wf.is_ok());
}

// ─── 3. Sub-workflow composition shares the same Deps ────────────────────────

#[test]
fn workflow_macro_flow_shares_deps() {
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "https://api".into(),
        }))
        .insert(Arc::new(Counter { factor: 2 }))
        .build();

    let child = expect_ok(
        workflow! {
            name: "child",
            codec: JsonCodec,
            deps: &deps,
            steps: [deps_multiply]
        },
        "child build",
    );

    expect_ok(
        workflow! {
            name: "parent",
            codec: JsonCodec,
            deps: &deps,
            steps: [deps_fetch, flow child]
        },
        "parent build",
    );
}

// ─── 4. Backward compatibility: workflow! without deps still works ──────────

#[test]
fn workflow_macro_without_deps_uses_new_for_plain_tasks() {
    // No `deps:` field — only tasks with no injected dependencies are usable.
    let wf: BuildResult = workflow! {
        name: "no-deps",
        codec: JsonCodec,
        steps: [deps_plain]
    };
    assert!(wf.is_ok());
}

// ─── 5. End-to-end execution with InProcessRunner ────────────────────────────

#[tokio::test]
async fn workflow_with_deps_executes_in_process() {
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "abc".into(),
        })) // len 3
        .insert(Arc::new(Counter { factor: 10 }))
        .build();

    let serializable = expect_ok(
        workflow! {
            name: "exec",
            codec: JsonCodec,
            deps: &deps,
            steps: [deps_fetch, deps_multiply, deps_plain]
        },
        "exec build",
    );

    let runner = InProcessRunner;
    // 5 + 3 = 8, 8 * 10 = 80, 80 + 1 = 81
    let status = runner.run(serializable.workflow(), 5u32).await.unwrap();
    assert!(matches!(
        status,
        sayiir_core::workflow::WorkflowStatus::Completed
    ));
}

// ─── 6. register_from_deps for hand-rolled task libraries / workers ─────────

#[test]
fn register_from_deps_populates_registry() {
    let codec = Arc::new(JsonCodec);
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "x".into(),
        }))
        .insert(Arc::new(Counter { factor: 4 }))
        .build();

    let mut registry = TaskRegistry::new();
    registry
        .register_from_deps::<DepsFetchTask, _>(codec.clone(), &deps)
        .unwrap();
    registry
        .register_from_deps::<DepsMultiplyTask, _>(codec.clone(), &deps)
        .unwrap();
    registry
        .register_from_deps::<DepsPlainTask, _>(codec, &deps)
        .unwrap();

    assert_eq!(registry.len(), 3);
    assert!(registry.contains("deps_fetch"));
    assert!(registry.contains("deps_multiply"));
    assert!(registry.contains("deps_plain"));
}

#[test]
fn register_from_deps_reports_missing() {
    let codec = Arc::new(JsonCodec);
    let deps = Deps::new(); // empty
    let mut registry = TaskRegistry::new();

    let err = registry
        .register_from_deps::<DepsFetchTask, _>(codec, &deps)
        .expect_err("missing HttpClient should be reported");
    assert_eq!(err.len(), 1);
    assert!(err[0].type_name.contains("HttpClient"));
    // Registry must not be partially populated.
    assert!(registry.is_empty());
}

#[test]
fn register_from_deps_no_inject_task_succeeds_with_empty_deps() {
    let codec = Arc::new(JsonCodec);
    let deps = Deps::new();
    let mut registry = TaskRegistry::new();

    registry
        .register_from_deps::<DepsPlainTask, _>(codec, &deps)
        .unwrap();
    assert!(registry.contains("deps_plain"));
}

#[test]
fn merged_deps_used_by_workflow_macro() {
    // Library-provided base container.
    let base = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "https://api".into(),
        }))
        .build();

    // Application layers on the Counter service before passing to workflow!.
    let mut deps = base;
    deps.merge(
        Deps::builder()
            .insert(Arc::new(Counter { factor: 9 }))
            .build(),
    );

    let wf: BuildResult = workflow! {
        name: "merged",
        codec: JsonCodec,
        deps: &deps,
        steps: [deps_fetch, deps_multiply]
    };
    assert!(wf.is_ok(), "merged Deps should satisfy both inject types");
}

#[test]
fn task_library_pattern() {
    // Demonstrate the documented "task library" pattern: a function that
    // returns a populated registry from a Deps container.
    fn billing_tasks(
        codec: Arc<JsonCodec>,
        deps: &Deps,
    ) -> Result<TaskRegistry, Vec<sayiir_core::deps::MissingDep>> {
        let mut reg = TaskRegistry::new();
        reg.register_from_deps::<DepsFetchTask, _>(codec.clone(), deps)?;
        reg.register_from_deps::<DepsMultiplyTask, _>(codec, deps)?;
        Ok(reg)
    }

    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "x".into(),
        }))
        .insert(Arc::new(Counter { factor: 1 }))
        .build();

    let reg = billing_tasks(Arc::new(JsonCodec), &deps).unwrap();
    assert_eq!(reg.len(), 2);
}

// ─── 7. registry: × deps: collision detection ───────────────────────────────

#[test]
fn registry_deps_conflict_is_detected() {
    // Pre-build a registry containing DepsFetchTask via the manual path.
    let codec = Arc::new(JsonCodec);
    let client_v1 = Arc::new(HttpClient {
        base_url: "v1".into(),
    });
    let mut prebuilt = TaskRegistry::new();
    DepsFetchTask::register(&mut prebuilt, codec.clone(), DepsFetchTask::new(client_v1));

    // Pass both `registry:` (with the pre-built task) AND `deps:` (which would
    // re-register the same task via from_deps). The macro must surface a
    // RegistryDepsConflict instead of silently dropping one source.
    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "v2".into(),
        }))
        .build();

    let errs = expect_err(
        workflow! {
            name: "conflict",
            codec: JsonCodec,
            registry: prebuilt,
            deps: &deps,
            steps: [deps_fetch]
        },
        "registry and deps both define deps_fetch",
    );
    let collected: Vec<_> = errs.into_iter().collect();
    assert_eq!(collected.len(), 1);
    match &collected[0] {
        BuildError::RegistryDepsConflict { task_id } => {
            assert_eq!(*task_id, "deps_fetch");
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn registry_without_collision_is_fine() {
    // Pre-built registry contains an unrelated task; `deps:` registers a
    // different one. No collision, build should succeed.
    let codec = Arc::new(JsonCodec);
    let mut prebuilt = TaskRegistry::new();
    DepsPlainTask::register(&mut prebuilt, codec, DepsPlainTask::new());

    let deps = Deps::builder()
        .insert(Arc::new(HttpClient {
            base_url: "x".into(),
        }))
        .build();

    expect_ok(
        workflow! {
            name: "no-conflict",
            codec: JsonCodec,
            registry: prebuilt,
            deps: &deps,
            steps: [deps_fetch]
        },
        "no-conflict build",
    );
}

#[test]
fn registry_collision_without_deps_is_not_flagged() {
    // When `deps:` is absent, the macro must not emit conflict checks —
    // pre-registering a task and then referencing it from `steps:` is a
    // documented override pattern (silent dedup retains the pre-built task).
    let codec = Arc::new(JsonCodec);
    let mut prebuilt = TaskRegistry::new();
    DepsPlainTask::register(&mut prebuilt, codec, DepsPlainTask::new());

    expect_ok(
        workflow! {
            name: "override-allowed",
            codec: JsonCodec,
            registry: prebuilt,
            steps: [deps_plain]
        },
        "override pattern build",
    );
}

#[test]
fn registry_deps_conflict_aggregates_with_missing_dep() {
    // Both error classes happen in the same build — the check loop must
    // collect both before returning.
    let codec = Arc::new(JsonCodec);
    let mut prebuilt = TaskRegistry::new();
    // `deps_plain` has no inject params; pre-register it to force a collision.
    DepsPlainTask::register(&mut prebuilt, codec, DepsPlainTask::new());

    // `deps_fetch` has #[inject] but `deps:` doesn't satisfy it → MissingDep.
    let deps = Deps::new();

    let errs = expect_err(
        workflow! {
            name: "both-errors",
            codec: JsonCodec,
            registry: prebuilt,
            deps: &deps,
            steps: [deps_plain, deps_fetch]
        },
        "missing dep + collision",
    );
    let collected: Vec<_> = errs.into_iter().collect();
    assert_eq!(collected.len(), 2);

    let has_missing = collected.iter().any(|e| {
        matches!(
            e,
            BuildError::MissingDep { task_id, .. } if *task_id == "deps_fetch"
        )
    });
    let has_conflict = collected.iter().any(|e| {
        matches!(
            e,
            BuildError::RegistryDepsConflict { task_id } if *task_id == "deps_plain"
        )
    });
    assert!(has_missing, "expected MissingDep for deps_fetch");
    assert!(has_conflict, "expected RegistryDepsConflict for deps_plain");
}
