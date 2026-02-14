//! JSON-based codec for Python objects.
//!
//! This module provides serialization and deserialization of Python objects
//! to/from bytes using JSON as the interchange format. It also handles
//! decoding JSON-encoded fork/join branch results into Python dicts.

use bytes::Bytes;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use sayiir_core::branch_results::NamedBranchResults;

/// Encodes a Python object to JSON bytes.
///
/// Uses Python's `json.dumps()` to serialize the object.
/// String constants are interned to avoid repeated Python string allocation.
pub fn encode_pyobject(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Bytes> {
    let json_mod = py.import(intern!(py, "json"))?;
    let json_str: String = json_mod
        .call_method1(intern!(py, "dumps"), (obj,))?
        .cast::<PyString>()?
        .extract()?;
    Ok(Bytes::from(json_str))
}

/// Decodes JSON bytes to a Python object.
///
/// Branch results (fork/join) are checked first because `serialize_branch_results`
/// now produces serde-JSON that `json.loads` would parse as a list-of-lists
/// instead of the dict that Python join tasks expect.
pub fn decode_to_pyobject(py: Python<'_>, bytes: &Bytes) -> PyResult<Py<PyAny>> {
    // Try branch results first: serde-JSON for NamedBranchResults is
    // [[name, [u8...]], ...] — a very specific shape that won't match
    // normal task inputs (numbers, strings, objects, flat arrays).
    if let Ok(named) = serde_json::from_slice::<NamedBranchResults>(bytes)
        && !named.is_empty() {
            return decode_branch_results_to_pydict(py, &named);
        }

    // Regular JSON path for normal task inputs
    let json_str = std::str::from_utf8(bytes)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
    let json_mod = py.import(intern!(py, "json"))?;
    Ok(json_mod
        .call_method1(intern!(py, "loads"), (json_str,))?
        .unbind())
}

/// Converts deserialized `NamedBranchResults` into a Python dict.
///
/// Each branch value is individually JSON-decoded (since `encode_pyobject`
/// produces valid JSON for each branch output).
fn decode_branch_results_to_pydict(
    py: Python<'_>,
    named: &NamedBranchResults,
) -> PyResult<Py<PyAny>> {
    let json_mod = py.import(intern!(py, "json"))?;
    let dict = PyDict::new(py);

    for (name, data) in named.as_slice() {
        let json_str = std::str::from_utf8(data)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e.to_string()))?;
        let val = json_mod.call_method1(intern!(py, "loads"), (json_str,))?;
        dict.set_item(name, val)?;
    }

    Ok(dict.into_any().unbind())
}

// Tests that require Python are run via pytest, not cargo test
// To run: cd python && maturin develop && pytest
