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
use napi::Env;
use napi::bindgen_prelude::*;
use sayiir_core::branch_results::NamedBranchResults;

/// Encodes a JavaScript value to JSON bytes via N-API serde bridge.
pub fn encode_js_value(env: &Env, value: Unknown) -> Result<Bytes> {
    let serde_val: serde_json::Value = env.from_js_value(value)?;
    let json_bytes = serde_json::to_vec(&serde_val)
        .map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    Ok(Bytes::from(json_bytes))
}

/// Decodes JSON bytes to a JavaScript value.
///
/// Branch results (fork/join) are checked first because `serialize_branch_results`
/// produces serde-JSON that `JSON.parse` would parse as an array-of-arrays
/// instead of the object that JS join tasks expect.
pub fn decode_to_js_value<'env>(env: &'env Env, bytes: &Bytes) -> Result<Unknown<'env>> {
    // Try branch results first
    if let Ok(named) = serde_json::from_slice::<NamedBranchResults>(bytes)
        && !named.is_empty()
    {
        return decode_branch_results(env, &named);
    }

    // Regular JSON path for normal task inputs
    let serde_val: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| Error::new(Status::InvalidArg, e.to_string()))?;
    env.to_js_value(&serde_val)
}

/// Converts deserialized `NamedBranchResults` into a JS object.
///
/// Each branch value is individually JSON-decoded.
fn decode_branch_results<'env>(
    env: &'env Env,
    named: &NamedBranchResults,
) -> Result<Unknown<'env>> {
    let mut obj = Object::new(env)?;

    for (name, data) in named.as_slice() {
        let serde_val: serde_json::Value = serde_json::from_slice(data)
            .map_err(|e| Error::new(Status::InvalidArg, e.to_string()))?;
        let js_val: Unknown = env.to_js_value(&serde_val)?;
        obj.set(name.as_str(), js_val)?;
    }

    obj.into_unknown(env)
}
