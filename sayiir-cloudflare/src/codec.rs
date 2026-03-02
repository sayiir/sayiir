//! Codec bridging JavaScript values and the Rust core's `Bytes`.
//!
//! Uses `JSON.stringify` / `JSON.parse` via `js_sys` for the JS ↔ Bytes
//! boundary, and simple byte-level string operations for assembling
//! structured JSON (branch envelopes, named results) from valid JSON
//! fragments — no `serde_json` needed.
//!
//! ## Fork/join branch results
//!
//! Branch results are encoded as a plain JSON object `{"name1":val1,...}`
//! by splicing together the already-valid JSON bytes of each branch output.

use bytes::Bytes;
use js_sys::JSON;
use wasm_bindgen::JsValue;

use crate::error::to_js_error;

/// Encode a JavaScript value to JSON bytes.
///
/// # Errors
///
/// Returns a JS error if `JSON.stringify` fails.
pub(crate) fn encode_js_value(value: &JsValue) -> Result<Bytes, JsValue> {
    let json_string =
        JSON::stringify(value).map_err(|_| to_js_error("Failed to stringify JS value"))?;
    let s: String = json_string.into();
    Ok(Bytes::from(s.into_bytes()))
}

/// Decode JSON bytes to a JavaScript value.
///
/// # Errors
///
/// Returns a JS error if the bytes are not valid UTF-8 or valid JSON.
pub(crate) fn decode_to_js_value(bytes: &Bytes) -> Result<JsValue, JsValue> {
    let json_str = std::str::from_utf8(bytes)
        .map_err(|e| to_js_error(format!("Invalid UTF-8 in bytes: {e}")))?;
    JSON::parse(json_str).map_err(|_| to_js_error(format!("Invalid JSON: {json_str}")))
}

/// Decode a JSON-encoded string from bytes (e.g. `"my_key"` → `my_key`).
///
/// # Errors
///
/// Returns an error if the bytes are not a valid JSON string.
pub(crate) fn decode_json_string(bytes: &[u8]) -> Result<String, JsValue> {
    let js_val = decode_to_js_value(&Bytes::copy_from_slice(bytes))?;
    js_val
        .as_string()
        .ok_or_else(|| to_js_error("Expected JSON string value"))
}

/// Build a branch envelope: `{"branch":"key","result":...}`.
///
/// # Errors
///
/// Returns a JS error if decoding result bytes or encoding the envelope fails.
pub(crate) fn encode_branch_envelope(key: &str, result_bytes: &[u8]) -> Result<Bytes, JsValue> {
    let obj = js_sys::Object::new();
    js_sys::Reflect::set(&obj, &JsValue::from_str("branch"), &JsValue::from_str(key))?;
    let result_val = decode_to_js_value(&Bytes::copy_from_slice(result_bytes))?;
    js_sys::Reflect::set(&obj, &JsValue::from_str("result"), &result_val)?;
    encode_js_value(&obj.into())
}

/// Merge named branch results into a JSON object: `{"name1":val1,"name2":val2,...}`.
///
/// # Errors
///
/// Returns a JS error if decoding any branch bytes or encoding the result fails.
pub(crate) fn encode_named_results(results: &[(String, Bytes)]) -> Result<Bytes, JsValue> {
    let obj = js_sys::Object::new();
    for (name, bytes) in results {
        let val = decode_to_js_value(&Bytes::copy_from_slice(bytes))?;
        js_sys::Reflect::set(&obj, &JsValue::from_str(name), &val)?;
    }
    encode_js_value(&obj.into())
}

/// Decode a `LoopResult` JSON (`{"_loop":"done"|"again","value":...}`)
/// into `(tag, inner_bytes)`.
///
/// # Errors
///
/// Returns a JS error if the JSON is malformed or missing required fields.
pub(crate) fn decode_loop_result(bytes: &[u8]) -> Result<(String, Bytes), JsValue> {
    let js_val = decode_to_js_value(&Bytes::copy_from_slice(bytes))?;
    let tag = js_sys::Reflect::get(&js_val, &JsValue::from_str("_loop"))?
        .as_string()
        .ok_or_else(|| to_js_error("Missing or invalid '_loop' tag in LoopResult"))?;
    let value = js_sys::Reflect::get(&js_val, &JsValue::from_str("value"))?;
    let value_bytes = encode_js_value(&value)?;
    Ok((tag, value_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::*;

    #[wasm_bindgen_test]
    fn roundtrip_number() {
        let val = JsValue::from_f64(42.0);
        let bytes = encode_js_value(&val).unwrap();
        assert_eq!(&bytes[..], b"42");
        let decoded = decode_to_js_value(&bytes).unwrap();
        assert_eq!(decoded.as_f64(), Some(42.0));
    }

    #[wasm_bindgen_test]
    fn roundtrip_string() {
        let val = JsValue::from_str("hello world");
        let bytes = encode_js_value(&val).unwrap();
        assert_eq!(&bytes[..], br#""hello world""#);
        let decoded = decode_to_js_value(&bytes).unwrap();
        assert_eq!(decoded.as_string().as_deref(), Some("hello world"));
    }

    #[wasm_bindgen_test]
    fn roundtrip_object() {
        let obj = js_sys::Object::new();
        js_sys::Reflect::set(&obj, &JsValue::from_str("a"), &JsValue::from_f64(1.0)).unwrap();
        js_sys::Reflect::set(&obj, &JsValue::from_str("b"), &JsValue::from_str("two")).unwrap();

        let bytes = encode_js_value(&obj.into()).unwrap();
        let decoded = decode_to_js_value(&bytes).unwrap();

        let a = js_sys::Reflect::get(&decoded, &JsValue::from_str("a")).unwrap();
        let b = js_sys::Reflect::get(&decoded, &JsValue::from_str("b")).unwrap();
        assert_eq!(a.as_f64(), Some(1.0));
        assert_eq!(b.as_string().as_deref(), Some("two"));
    }

    #[wasm_bindgen_test]
    fn roundtrip_null() {
        let bytes = encode_js_value(&JsValue::NULL).unwrap();
        assert_eq!(&bytes[..], b"null");
        let decoded = decode_to_js_value(&bytes).unwrap();
        assert!(decoded.is_null());
    }

    #[wasm_bindgen_test]
    fn roundtrip_bool() {
        let bytes = encode_js_value(&JsValue::TRUE).unwrap();
        assert_eq!(&bytes[..], b"true");
        let decoded = decode_to_js_value(&bytes).unwrap();
        assert_eq!(decoded.as_bool(), Some(true));
    }

    #[wasm_bindgen_test]
    fn roundtrip_array() {
        let arr = js_sys::Array::new();
        arr.push(&JsValue::from_f64(1.0));
        arr.push(&JsValue::from_str("x"));

        let bytes = encode_js_value(&arr.into()).unwrap();
        let decoded = decode_to_js_value(&bytes).unwrap();

        let js_arr = js_sys::Array::from(&decoded);
        assert_eq!(js_arr.length(), 2);
        assert_eq!(js_arr.get(0).as_f64(), Some(1.0));
        assert_eq!(js_arr.get(1).as_string().as_deref(), Some("x"));
    }

    #[wasm_bindgen_test]
    fn decode_string_from_json_bytes() {
        let bytes = br#""my_key""#;
        assert_eq!(decode_json_string(bytes).unwrap(), "my_key");
    }

    #[wasm_bindgen_test]
    fn decode_string_rejects_number() {
        assert!(decode_json_string(b"42").is_err());
    }

    #[wasm_bindgen_test]
    fn branch_envelope_roundtrip() {
        let result_bytes = b"123";
        let envelope = encode_branch_envelope("my_branch", result_bytes).unwrap();

        let decoded = decode_to_js_value(&envelope).unwrap();
        let branch = js_sys::Reflect::get(&decoded, &JsValue::from_str("branch")).unwrap();
        let result = js_sys::Reflect::get(&decoded, &JsValue::from_str("result")).unwrap();

        assert_eq!(branch.as_string().as_deref(), Some("my_branch"));
        assert_eq!(result.as_f64(), Some(123.0));
    }

    #[wasm_bindgen_test]
    fn branch_envelope_with_object_result() {
        let inner = br#"{"x":1}"#;
        let envelope = encode_branch_envelope("b1", inner).unwrap();
        let decoded = decode_to_js_value(&envelope).unwrap();

        let result = js_sys::Reflect::get(&decoded, &JsValue::from_str("result")).unwrap();
        let x = js_sys::Reflect::get(&result, &JsValue::from_str("x")).unwrap();
        assert_eq!(x.as_f64(), Some(1.0));
    }

    #[wasm_bindgen_test]
    fn named_results_roundtrip() {
        let results = vec![
            ("alpha".to_string(), Bytes::from(r#""hello""#)),
            ("beta".to_string(), Bytes::from("42")),
        ];
        let encoded = encode_named_results(&results).unwrap();
        let decoded = decode_to_js_value(&encoded).unwrap();

        let alpha = js_sys::Reflect::get(&decoded, &JsValue::from_str("alpha")).unwrap();
        let beta = js_sys::Reflect::get(&decoded, &JsValue::from_str("beta")).unwrap();
        assert_eq!(alpha.as_string().as_deref(), Some("hello"));
        assert_eq!(beta.as_f64(), Some(42.0));
    }

    #[wasm_bindgen_test]
    fn named_results_empty() {
        let encoded = encode_named_results(&[]).unwrap();
        assert_eq!(&encoded[..], b"{}");
    }

    #[wasm_bindgen_test]
    fn loop_result_done() {
        let bytes = br#"{"_loop":"done","value":99}"#;
        let (tag, inner) = decode_loop_result(bytes).unwrap();
        assert_eq!(tag, "done");
        assert_eq!(&inner[..], b"99");
    }

    #[wasm_bindgen_test]
    fn loop_result_again_with_object() {
        let bytes = br#"{"_loop":"again","value":{"count":5}}"#;
        let (tag, inner) = decode_loop_result(bytes).unwrap();
        assert_eq!(tag, "again");

        let decoded = decode_to_js_value(&inner).unwrap();
        let count = js_sys::Reflect::get(&decoded, &JsValue::from_str("count")).unwrap();
        assert_eq!(count.as_f64(), Some(5.0));
    }

    #[wasm_bindgen_test]
    fn loop_result_missing_tag() {
        let bytes = br#"{"value":1}"#;
        assert!(decode_loop_result(bytes).is_err());
    }
}
