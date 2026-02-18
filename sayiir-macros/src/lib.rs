#![deny(missing_docs)]
//! Procedural macros for the Sayiir durable workflow engine.
//!
//! Provides two macros that eliminate boilerplate when defining workflows:
//!
//! - **`#[task]`** — Transforms an async function into a `CoreTask` struct with
//!   automatic registration, metadata, and dependency injection.
//!
//! - **`workflow!`** — Builds a workflow pipeline with a concise DSL that desugars
//!   to `WorkflowBuilder` method calls.
//!
//! # Quick Example
//!
//! ```rust,ignore
//! use sayiir_macros::task;
//!
//! #[task(timeout = "30s", retries = 3, backoff = "100ms")]
//! async fn charge(order: Order, #[inject] stripe: Arc<Stripe>) -> Result<Receipt, BoxError> {
//!     stripe.charge(&order).await
//! }
//!
//! let workflow = workflow! {
//!     name: "order-process",
//!     codec: JsonCodec,
//!     steps: [
//!         validate(order: Order) { validate_order(order) },
//!         charge,
//!         (send_email || update_inventory),
//!         finalize,
//!     ]
//! };
//! ```

mod branch_key;
mod task;
mod util;
mod workflow;

/// Transforms an async function into a `CoreTask` implementation.
///
/// # Attributes
///
/// - `id = "stable_name"` — **strongly recommended**: set an explicit, stable task ID.
///   The default (function name) ties your workflow identity to code structure — renaming
///   the function silently changes the ID, breaking in-flight workflows on resume.
///   Always set `id` in production workflows.
/// - `display_name = "Charge Card"` — human-readable name
/// - `description = "Charges the customer's card"` — task description
/// - `timeout = "30s"` — task timeout (supports `ms`, `s`, `m`, `h` suffixes)
/// - `retries = 3` — maximum retry count
/// - `backoff = "100ms"` — initial retry delay
/// - `backoff_multiplier = 2.0` — exponential multiplier (default: 2.0)
/// - `tags = "io"` — categorization tags (can be repeated)
///
/// # Parameters
///
/// - Exactly **one** non-`#[inject]` parameter: the task input type
/// - Zero or more `#[inject]` parameters: dependency-injected fields
///
/// # Return Types
///
/// - `Result<T, E>` — fallible; `E` is converted via `Into<BoxError>`
/// - `T` — infallible; automatically wrapped in `Ok(...)`
///
/// # Generated Code
///
/// - A PascalCase struct with `Task` suffix (e.g., `fn charge` → `struct ChargeTask`)
/// - `new()` constructor with positional args for injected dependencies
/// - `task_id()` — returns the task ID (explicit `id` or function name)
/// - `metadata()` — returns `TaskMetadata` built from attributes
/// - `register()` method for `TaskRegistry` integration
/// - `CoreTask` trait implementation
/// - The original function is preserved for direct use/testing
///
/// # Example
///
/// ```rust,ignore
/// #[task(id = "charge_card", timeout = "30s", retries = 3)]
/// async fn charge(order: Order, #[inject] stripe: Arc<Stripe>) -> Result<Receipt, BoxError> {
///     stripe.charge(&order).await
/// }
///
/// // Generated: ChargeTask with new(stripe), task_id() → "charge_card", etc.
/// let task = ChargeTask::new(stripe);
/// ChargeTask::register(&mut registry, codec, task);
/// ```
#[proc_macro_attribute]
pub fn task(
    attr: proc_macro::TokenStream,
    item: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    match task::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Derives `BranchKey` for a fieldless enum.
///
/// Each variant maps to a `snake_case` string key by default. Use
/// `#[branch_key("custom_key")]` on a variant to override.
///
/// # Example
///
/// ```rust,ignore
/// use sayiir_macros::BranchKey;
///
/// #[derive(BranchKey)]
/// enum Intent {
///     Billing,           // key = "billing"
///     TechSupport,       // key = "tech_support"
///     #[branch_key("other")]
///     Fallback,          // key = "other"
/// }
/// ```
#[proc_macro_derive(BranchKey, attributes(branch_key))]
pub fn derive_branch_key(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as syn::DeriveInput);
    match branch_key::expand(input) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Builds a workflow pipeline with a concise DSL.
///
/// # Syntax
///
/// ```text
/// workflow! {
///     name: "workflow-id",
///     codec: CodecType,
///     registry: registry_expr,  // optional — defaults to TaskRegistry::new()
///     steps: [step, step, step]
/// }
/// ```
///
/// # Step Types
///
/// - `task_name` — reference to a `#[task]`-generated struct (resolved as `TaskNameTask`)
/// - `name(param: Type) { expr }` — inline task
/// - `(step || step), join` — parallel fork
/// - `delay "5s"` — durable delay (auto-generated ID)
/// - `delay "wait_24h" "5s"` — durable delay with custom ID
/// - `signal "name"` — wait for external signal
/// - `signal "name" timeout "30s"` — signal with timeout
/// - `route key_fn { "k" => [steps], _ => [steps] }` — conditional routing (string keys)
/// - `route key_fn -> Enum { Variant => [steps], _ => [steps] }` — typed conditional routing
///
/// # Returns
///
/// A `Result<SerializableWorkflow<C, Input, ()>, WorkflowError>` expression.
#[proc_macro]
pub fn workflow(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    match workflow::expand(input.into()) {
        Ok(tokens) => tokens.into(),
        Err(e) => e.to_compile_error().into(),
    }
}
