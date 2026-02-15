//! Node.js-exposed workflow engine.
//!
//! Rust drives all execution logic. JavaScript only provides task implementations
//! via a callback object passed to `run()`.

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi::{Env, JsBoolean, JsFunction, JsObject, JsUnknown};
use napi_derive::napi;
use std::sync::Arc;

use sayiir_runtime::execute_continuation_sync;

use crate::codec::{decode_to_js_value, encode_js_value};
use crate::flow::NapiWorkflow;

/// Workflow engine for simple (non-durable) execution.
#[napi]
pub struct NapiWorkflowEngine;

#[napi]
impl NapiWorkflowEngine {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self
    }

    /// Run a workflow to completion.
    ///
    /// `task_registry` is an object mapping `task_id` -> callable function.
    #[napi]
    pub fn run(
        &self,
        env: Env,
        workflow: &NapiWorkflow,
        input: JsUnknown,
        task_registry: JsObject,
    ) -> Result<JsUnknown> {
        let input_bytes = encode_js_value(&env, &input)?;
        let continuation = Arc::clone(&workflow.continuation);

        tracing::info!(workflow_id = %workflow.workflow_id, "starting workflow execution");

        let result = execute_continuation_sync(&continuation, input_bytes, &|task_id, input| {
            execute_js_task(&env, task_id, &input, &task_registry).map_err(|e| {
                let msg: sayiir_core::error::BoxError = e.to_string().into();
                msg
            })
        })
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;

        tracing::info!(workflow_id = %workflow.workflow_id, "workflow execution completed");

        decode_to_js_value(&env, &result)
    }
}

/// Execute a JavaScript task by calling it from the registry.
pub(crate) fn execute_js_task(
    env: &Env,
    task_id: &str,
    input: &Bytes,
    registry: &JsObject,
) -> Result<Bytes> {
    let callable: JsFunction = registry.get_named_property(task_id).map_err(|_| {
        Error::new(
            Status::GenericFailure,
            format!("Task '{task_id}' not found in registry"),
        )
    })?;

    tracing::debug!(task_id, input_bytes = input.len(), "executing js task");

    let input_obj = decode_to_js_value(env, input)?;
    let result: JsUnknown = callable.call(None, &[input_obj])?;

    // Check if the result is a Promise (has a .then method that is a function)
    let result = if is_promise(&result)? {
        tracing::debug!(task_id, "task returned promise, awaiting synchronously");
        await_promise_sync(env, &result)?
    } else {
        result
    };

    tracing::debug!(task_id, "js task completed");

    encode_js_value(env, &result)
}

/// Check if a JS value is a Promise (has a `.then` property that is a function).
fn is_promise(value: &JsUnknown) -> Result<bool> {
    let value_type = value.get_type()?;
    if value_type != ValueType::Object {
        return Ok(false);
    }
    let obj = unsafe { value.cast::<JsObject>() };
    match obj.get_named_property::<JsUnknown>("then") {
        Ok(then_val) => Ok(then_val.get_type()? == ValueType::Function),
        Err(_) => Ok(false),
    }
}

/// Synchronously await a Promise by attaching `.then()`/`.catch()` callbacks
/// and draining microtasks via `env.run_script`.
///
/// This works because in the synchronous engine we run on the main JS thread,
/// and resolved/rejected promise callbacks are microtasks that get processed
/// when we re-enter the JS engine.
fn await_promise_sync(env: &Env, promise: &JsUnknown) -> Result<JsUnknown> {
    let obj = unsafe { promise.cast::<JsObject>() };
    let then_fn: JsFunction = obj.get_named_property("then")?;

    // Create a container object to store the result
    let container: JsObject =
        env.run_script("({ value: undefined, error: undefined, done: false })")?;

    // Create references for the callbacks (Ref<()> requires create_reference)
    let resolve_ref = env.create_reference(&container)?;
    let reject_ref = env.create_reference(&container)?;

    // Create resolve callback
    let resolve_cb = env.create_function_from_closure("resolve", move |ctx| {
        let mut container: JsObject = ctx.env.get_reference_value(&resolve_ref)?;
        let val = ctx.get::<JsUnknown>(0)?;
        container.set_named_property("value", val)?;
        container.set_named_property("done", ctx.env.get_boolean(true)?)?;
        ctx.env.get_undefined()
    })?;

    // Create reject callback
    let reject_cb = env.create_function_from_closure("reject", move |ctx| {
        let mut container: JsObject = ctx.env.get_reference_value(&reject_ref)?;
        let val = ctx.get::<JsUnknown>(0)?;
        container.set_named_property("error", val)?;
        container.set_named_property("done", ctx.env.get_boolean(true)?)?;
        ctx.env.get_undefined()
    })?;

    // Attach callbacks: promise.then(resolve, reject)
    then_fn.call(
        Some(&obj),
        &[resolve_cb.into_unknown(), reject_cb.into_unknown()],
    )?;

    // Drain microtasks — resolved promise callbacks fire as microtasks
    // which are processed when we call back into the JS engine
    let _: JsUnknown = env.run_script("void 0")?;

    let done: bool = container
        .get_named_property::<JsBoolean>("done")?
        .get_value()?;

    if !done {
        return Err(Error::new(
            Status::GenericFailure,
            "Async task did not resolve synchronously. \
             Use the durable engine for truly async tasks.",
        ));
    }

    let error: JsUnknown = container.get_named_property("error")?;
    if error.get_type()? != ValueType::Undefined {
        let error_str = error.coerce_to_string()?;
        return Err(Error::new(
            Status::GenericFailure,
            error_str.into_utf8()?.into_owned()?,
        ));
    }

    container.get_named_property("value")
}
