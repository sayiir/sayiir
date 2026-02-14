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
//! let workflow = workflow!("order-process", JsonCodec, registry,
//!     validate(order: Order) { validate_order(order) }
//!     => charge
//!     => send_email || update_inventory
//!     => finalize
//! );
//! ```

mod task;
mod util;
mod workflow;

/// Transforms an async function into a `CoreTask` implementation.
///
/// # Attributes
///
/// - `id = "custom_name"` — override the task ID (default: function name)
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
/// # Generated Code
///
/// - A PascalCase struct (e.g., `fn charge` → `struct Charge`)
/// - `new()` constructor with positional args for injected dependencies
/// - `task_id()` and `metadata()` helper methods
/// - `register()` method for `TaskRegistry` integration
/// - `CoreTask` trait implementation
/// - The original function is preserved for direct use/testing
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

/// Builds a workflow pipeline with a concise DSL.
///
/// # Syntax
///
/// ```text
/// workflow!("workflow-id", CodecType, registry_expr,
///     step => step => step
/// )
/// ```
///
/// # Step Types
///
/// - `task_name` — reference to a `#[task]`-generated struct
/// - `name(param: Type) { expr }` — inline task
/// - `step || step` — parallel fork (branches)
/// - `delay "5s"` — durable delay
/// - `=>` — sequential chain (or join after `||`)
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
