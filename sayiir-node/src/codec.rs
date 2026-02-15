//! JSON-based codec bridging JavaScript values and the Rust core's `Bytes` representation.
//!
//! The Rust workflow engine operates entirely on opaque `Bytes` — it never inspects
//! task inputs or outputs. The Node.js binding layer must convert between
//! JavaScript values and `Bytes` at two boundaries:
//!
//! 1. **Encode** — before handing a JS return value to the Rust engine
//! 2. **Decode** — before passing stored `Bytes` back into a JS task
//!
//! ## Fork/join branch results
//!
//! Fork/join branches produce a `NamedBranchResults` (serialized by serde as
//! `[[name, [u8…]], …]`). If decoded naively with `JSON.parse`, this would yield
//! an array-of-arrays instead of the `Record<string, value>` that JS join tasks
//! expect. [`decode_to_js_value`] detects this shape and converts it into a JS
//! object where each value is individually JSON-decoded.

use bytes::Bytes;
use napi::bindgen_prelude::*;
use napi::{Env, JsFunction, JsObject, JsUnknown};
use sayiir_core::branch_results::NamedBranchResults;

/// Encodes a JavaScript value to JSON bytes via `JSON.stringify`.
pub fn encode_js_value(env: &Env, value: &JsUnknown) -> Result<Bytes> {
    let global = env.get_global()?;
    let json: JsObject = global.get_named_property("JSON")?;
    let stringify_fn: JsFunction = json.get_named_property("stringify")?;
    let stringify_result = stringify_fn
        .call(Some(&json), &[value])?
        .coerce_to_string()?;
    let json_str = stringify_result.into_utf8()?.into_owned()?;
    Ok(Bytes::from(json_str))
}

/// Decodes JSON bytes to a JavaScript value.
///
/// Branch results (fork/join) are checked first because `serialize_branch_results`
/// produces serde-JSON that `JSON.parse` would parse as an array-of-arrays
/// instead of the object that JS join tasks expect.
pub fn decode_to_js_value(env: &Env, bytes: &Bytes) -> Result<JsUnknown> {
    // Try branch results first
    if let Ok(named) = serde_json::from_slice::<NamedBranchResults>(bytes)
        && !named.is_empty()
    {
        return decode_branch_results_to_js_object(env, &named);
    }

    // Regular JSON path for normal task inputs
    let json_str =
        std::str::from_utf8(bytes).map_err(|e| Error::new(Status::InvalidArg, e.to_string()))?;
    json_parse(env, json_str)
}

/// Call `JSON.parse(str)` and return the result.
fn json_parse(env: &Env, s: &str) -> Result<JsUnknown> {
    let global = env.get_global()?;
    let json: JsObject = global.get_named_property("JSON")?;
    let parse_fn: JsFunction = json.get_named_property("parse")?;
    let js_str = env.create_string(s)?;
    parse_fn.call(Some(&json), &[js_str])
}

/// Converts deserialized `NamedBranchResults` into a JS object.
///
/// Each branch value is individually JSON-decoded.
fn decode_branch_results_to_js_object(env: &Env, named: &NamedBranchResults) -> Result<JsUnknown> {
    let mut obj = env.create_object()?;

    for (name, data) in named.as_slice() {
        let json_str =
            std::str::from_utf8(data).map_err(|e| Error::new(Status::InvalidArg, e.to_string()))?;
        let val = json_parse(env, json_str)?;
        obj.set_named_property(name, val)?;
    }

    Ok(obj.into_unknown())
}
