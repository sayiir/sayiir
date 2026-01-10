pub mod context;
pub mod primitives;
pub mod serialization;
pub mod task;

/// Macro to convert an async function into a CoreTask.
///
/// This macro automatically wraps input/output types in `Json<T>` to provide
/// clean function signatures. By default, it detects if types are already wrappers
/// (like `Json<T>`, `Bincode<T>`, or custom wrappers) and won't double-wrap them.
///
/// # Default behavior (automatic JSON wrapping)
///
/// ```rust
/// use workflow_core::task;
/// use workflow_core::task::task;
///
/// #[task]
/// async fn my_task(input: String) -> Result<i32, anyhow::Error> {
///     // The macro transforms this function signature to use Json<T> internally.
///     // You write clean code with plain types, and the macro handles the wrapping.
///     // After transformation: async fn my_task(input: Json<String>) -> Result<Json<i32>, anyhow::Error>
///     // The macro also transforms the body to wrap/unwrap automatically.
///     // This example shows what you write - the macro handles the Json wrapping.
///     todo!("Example - return value would be automatically wrapped by the macro")
/// }
///
/// // The transformed function can be used with the task() function
/// # fn main() {
/// # let _task = task(my_task);
/// # }
/// ```
///
/// # Custom wrappers (automatic detection)
///
/// If you use a custom wrapper that implements `TaskInput`/`TaskOutput`, the macro
/// will detect it and won't wrap it:
///
/// ```rust
/// use workflow_core::task;
/// use workflow_core::serialization::json::Json;
/// use workflow_core::serialization::TaskInput;
/// use workflow_core::task::task;
/// use bytes::Bytes;
///
/// #[task]
/// async fn my_task(input: Json<String>) -> Result<Json<i32>, anyhow::Error> {
///     // Json<T> is detected as a wrapper, so it's not wrapped again
///     let value = input.as_ref();
///     let result = value.len() as i32;
///     // Construct Json using TaskInput::from_bytes
///     let json_bytes = Bytes::from(serde_json::to_vec(&result)?);
///     Ok(Json::from_bytes(json_bytes)?)
/// }
///
/// let _task = task(my_task);
/// ```
///
/// # Custom serialization formats
///
/// You can use custom serialization formats by opting out of automatic JSON wrapping:
///
/// ```rust
/// use workflow_core::task;
/// use workflow_core::serialization::json::Json;
/// use workflow_core::serialization::TaskInput;
/// use workflow_core::task::task;
/// use bytes::Bytes;
///
/// #[task(custom_serialization)]
/// async fn my_task(input: Json<String>) -> Result<Json<i32>, anyhow::Error> {
///     // Custom serialization - types are used exactly as specified without automatic wrapping
///     let value = input.as_ref();
///     let result = value.len() as i32;
///     // Construct Json using TaskInput::from_bytes
///     let json_bytes = Bytes::from(serde_json::to_vec(&result)?);
///     Ok(Json::from_bytes(json_bytes)?)
/// }
///
/// let _task = task(my_task);
/// ```
///
/// See the [serialization module](crate::serialization) for more details on
/// custom serialization formats.
#[doc(inline)]
pub use workflow_macros::task;
