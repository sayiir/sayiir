//! Error mapping from `JsValue` and internal errors to `wasm_bindgen::JsValue`.

use wasm_bindgen::JsValue;

/// Convert an arbitrary error into a `JsValue` for wasm-bindgen.
pub(crate) fn to_js_error(msg: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&msg.to_string())
}

/// Convert a `sayiir_persistence::BackendError` into a `JsValue`.
pub(crate) fn backend_err(e: sayiir_persistence::BackendError) -> JsValue {
    to_js_error(e)
}
