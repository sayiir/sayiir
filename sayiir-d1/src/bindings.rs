//! Minimal `wasm_bindgen` FFI declarations for the Cloudflare D1 API.

use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// D1Database
// ---------------------------------------------------------------------------

#[wasm_bindgen]
extern "C" {
    /// A Cloudflare D1 database handle obtained from the Worker binding.
    #[wasm_bindgen(js_name = "D1Database")]
    pub type D1Database;

    /// Create a prepared statement from a SQL string.
    #[wasm_bindgen(method)]
    pub fn prepare(this: &D1Database, query: &str) -> D1PreparedStatement;

    /// Run a batch of prepared statements in a single round-trip.
    /// Returns a `Promise<D1Result[]>`.
    #[wasm_bindgen(method)]
    pub fn batch(this: &D1Database, statements: &js_sys::Array) -> js_sys::Promise;

    /// Run one or more semicolon-separated SQL statements (DDL only).
    /// Returns a `Promise<D1ExecResult>`.
    #[wasm_bindgen(method, js_name = "exec")]
    pub fn run_raw(this: &D1Database, query: &str) -> js_sys::Promise;
}

// ---------------------------------------------------------------------------
// D1PreparedStatement
// ---------------------------------------------------------------------------

#[wasm_bindgen]
extern "C" {
    /// A prepared D1 statement with optional bound parameters.
    #[wasm_bindgen(js_name = "D1PreparedStatement")]
    pub type D1PreparedStatement;

    /// Bind positional parameters. Accepts a variadic JS rest parameter;
    /// we pass a spread-compatible array via `js_sys::Array`.
    #[wasm_bindgen(method, variadic)]
    pub fn bind(this: &D1PreparedStatement, values: &js_sys::Array) -> D1PreparedStatement;

    /// Return the first matching row (or null). Returns `Promise<Object | null>`.
    #[wasm_bindgen(method)]
    pub fn first(this: &D1PreparedStatement) -> js_sys::Promise;

    /// Return all matching rows. Returns `Promise<D1Result>`.
    #[wasm_bindgen(method)]
    pub fn all(this: &D1PreparedStatement) -> js_sys::Promise;

    /// Run a mutating statement. Returns `Promise<D1Result>`.
    #[wasm_bindgen(method)]
    pub fn run(this: &D1PreparedStatement) -> js_sys::Promise;
}

/// Read a string column from a JS row object.
pub(crate) fn get_str_col(row: &JsValue, col: &str) -> Option<String> {
    let val = js_sys::Reflect::get(row, &JsValue::from_str(col)).ok()?;
    val.as_string()
}

/// Read a BLOB column from a JS row object.
///
/// D1 returns BLOB columns as `ArrayBuffer`; we convert via `Uint8Array`.
pub(crate) fn get_blob_col(row: &JsValue, col: &str) -> Option<Vec<u8>> {
    let val = js_sys::Reflect::get(row, &JsValue::from_str(col)).ok()?;
    if val.is_null() || val.is_undefined() {
        return None;
    }
    // D1 may return an ArrayBuffer or a Uint8Array depending on the binding.
    let array = if val.is_instance_of::<js_sys::Uint8Array>() {
        js_sys::Uint8Array::from(val)
    } else {
        js_sys::Uint8Array::new(&val)
    };
    Some(array.to_vec())
}

/// Read an integer column from a JS row object.
#[allow(dead_code)]
pub(crate) fn get_i32_col(row: &JsValue, col: &str) -> Option<i32> {
    let val = js_sys::Reflect::get(row, &JsValue::from_str(col)).ok()?;
    #[allow(clippy::cast_possible_truncation)]
    val.as_f64().map(|f| f as i32)
}
