//! Node.js-exposed workflow engine.
//!
//! Rust drives all execution logic. JavaScript only provides task implementations
//! via a callback object passed to `run()`.

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi::{Env, JsBoolean, JsFunction, JsObject, JsUnknown};
use napi_derive::napi;
use std::sync::Arc;

// libuv FFI — Node.js links libuv statically, so these symbols are always available.
const UV_RUN_DEFAULT: std::ffi::c_int = 0;
const UV_RUN_ONCE: std::ffi::c_int = 1;
const UV_RUN_NOWAIT: std::ffi::c_int = 2;

unsafe extern "C" {
    fn uv_run(loop_: *mut napi::sys::uv_loop_s, mode: std::ffi::c_int) -> std::ffi::c_int;
}

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

/// Get the libuv event loop handle from the napi environment.
#[allow(clippy::borrow_as_ptr)]
fn get_uv_event_loop(env: &Env) -> Result<*mut napi::sys::uv_loop_s> {
    let mut uv_loop: *mut napi::sys::uv_loop_s = std::ptr::null_mut();
    let status = unsafe { napi::sys::napi_get_uv_event_loop(env.raw(), &mut uv_loop) };
    if status != napi::sys::Status::napi_ok {
        return Err(Error::new(
            Status::GenericFailure,
            "Failed to get libuv event loop",
        ));
    }
    if uv_loop.is_null() {
        return Err(Error::new(
            Status::GenericFailure,
            "libuv event loop is null",
        ));
    }
    Ok(uv_loop)
}

/// Await a JS Promise by pumping the libuv event loop.
///
/// Attaches `.then()`/`.catch()` callbacks to the promise, then runs
/// `uv_run(UV_RUN_ONCE)` in a loop until the promise settles. This allows
/// truly async tasks (fetch, timers, file I/O) to complete — not just
/// microtask-only promises.
///
/// SAFETY: Must be called on the main JS thread (guaranteed by our
/// `current_thread` tokio runtime + `block_on`).
fn await_promise_sync(env: &Env, promise: &JsUnknown) -> Result<JsUnknown> {
    let obj = unsafe { promise.cast::<JsObject>() };
    let then_fn: JsFunction = obj.get_named_property("then")?;

    // Container to store the result across callbacks
    let container: JsObject =
        env.run_script("({ value: undefined, error: undefined, done: false })")?;

    let resolve_ref = env.create_reference(&container)?;
    let reject_ref = env.create_reference(&container)?;

    let resolve_cb = env.create_function_from_closure("resolve", move |ctx| {
        let mut container: JsObject = ctx.env.get_reference_value(&resolve_ref)?;
        let val = ctx.get::<JsUnknown>(0)?;
        container.set_named_property("value", val)?;
        container.set_named_property("done", ctx.env.get_boolean(true)?)?;
        ctx.env.get_undefined()
    })?;

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

    // Pump the libuv event loop until the promise settles.
    //
    // Challenge: V8 microtasks (Promise.then callbacks) are NOT drained by
    // libuv — they're a V8 concept. But `uv_run` triggers microtask draining
    // as part of each event loop iteration. The trick: we need at least one
    // libuv handle active for `uv_run` to process a tick (otherwise it
    // returns immediately with alive=0).
    //
    // Strategy:
    // 1. Create a `setImmediate` sentinel → ensures a libuv "check" handle
    //    exists so `uv_run(UV_RUN_ONCE)` processes one full tick, draining
    //    microtasks in the process. This handles microtask-only promises.
    // 2. If the promise didn't settle, there's real async I/O pending (timer,
    //    fetch, fs). Call `uv_run(UV_RUN_ONCE)` without a sentinel — it
    //    blocks until the I/O completes, then we repeat from step 1 to drain
    //    the resulting microtasks.
    let uv_loop = get_uv_event_loop(env)?;

    loop {
        // Drain microtasks: setImmediate creates a check handle that fires
        // on the next tick, ensuring uv_run processes at least one iteration.
        let _: JsUnknown = env.run_script("setImmediate(()=>{})")?;
        unsafe { uv_run(uv_loop, UV_RUN_ONCE) };

        let done: bool = container
            .get_named_property::<JsBoolean>("done")?
            .get_value()?;
        if done {
            break;
        }

        // Promise needs real I/O — block until something fires.
        let alive = unsafe { uv_run(uv_loop, UV_RUN_ONCE) };
        if alive == 0 {
            return Err(Error::new(
                Status::GenericFailure,
                "Async task promise did not resolve — \
                 event loop drained with no pending work.",
            ));
        }
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
