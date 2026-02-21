use proc_macro2::Span;
use syn::{Expr, Ident, LitStr, Type};

use crate::task::duration::DurationLit;

/// A branch arm key: either a string literal or a typed enum variant.
#[derive(Debug)]
pub enum BranchArmKey {
    /// String literal key: `"billing"`
    Literal(LitStr),
    /// Enum variant name: `Billing` (qualified as `KeyType::Billing` in codegen).
    Variant(Ident),
}

/// Top-level workflow definition parsed from `workflow! { ... }`.
#[derive(Debug)]
pub struct WorkflowDef {
    /// Workflow ID string literal.
    pub id: syn::LitStr,
    /// Codec type path.
    pub codec: syn::Path,
    /// Registry expression (optional — defaults to `TaskRegistry::new()`).
    pub registry: Option<Expr>,
    /// Metadata expression (optional — defaults to `()`).
    pub metadata: Option<Expr>,
    /// Pipeline steps.
    pub steps: Vec<WorkflowStep>,
}

/// A single step in the workflow pipeline.
#[derive(Debug)]
pub enum WorkflowStep {
    /// Reference to a `#[task]`-generated struct by function name.
    /// e.g. `charge` → resolves to `ChargeTask`
    TaskRef { struct_name: Ident, span: Span },

    /// Inline task with a closure body.
    /// e.g. `validate(order: Order) { validate(order) }`
    InlineTask {
        name: Ident,
        param_name: Ident,
        param_type: Box<Type>,
        body: Expr,
        span: Span,
    },

    /// Parallel fork: multiple steps that run concurrently.
    /// e.g. `send_email || update_inventory`
    Parallel {
        branches: Vec<WorkflowStep>,
        span: Span,
    },

    /// Durable delay.
    /// e.g. `delay "5s"`
    Delay {
        id: String,
        duration: DurationLit,
        span: Span,
    },

    /// Wait for an external signal.
    /// e.g. `signal "approval"` or `signal "my_id" "approval" timeout "30s"`
    AwaitSignal {
        id: String,
        signal_name: String,
        timeout: Option<DurationLit>,
        span: Span,
    },

    /// Loop node whose body repeats until the task returns `LoopResult::Done`.
    /// e.g. `loop refine_task 10` or `loop refine_task 10 exit_with_last`
    Loop {
        /// Body task reference (struct name, PascalCase).
        body: Ident,
        /// Maximum number of iterations.
        max_iterations: syn::LitInt,
        /// What to do when `max_iterations` is reached (defaults to `Fail`).
        on_max: Option<Ident>,
        span: Span,
    },

    /// Conditional branch based on a routing key.
    /// e.g. `route classify_key { "billing" => [handle_billing], _ => [fallback] }`
    /// or   `route classify_key -> Intent { Billing => [handle_billing], _ => [fallback] }`
    Route {
        /// Branch node ID (positional: `"branch_0"`, `"branch_1"`, etc.).
        id: String,
        /// Key function task reference (struct name, PascalCase).
        key_fn: Ident,
        /// Optional `BranchKey` enum type for typed routing keys.
        key_type: Option<syn::Path>,
        /// Named branches: `(key, steps_pipeline)`.
        branches: Vec<(BranchArmKey, Vec<WorkflowStep>)>,
        /// Optional default branch.
        default: Option<Vec<WorkflowStep>>,
        span: Span,
    },
}

/// Assign positional IDs (`branch_0`, `branch_1`, …) to all `Route` nodes.
pub fn renumber_branches(steps: &mut [WorkflowStep]) {
    let mut counter = 0usize;
    renumber_branches_inner(steps, &mut counter);
}

fn renumber_branches_inner(steps: &mut [WorkflowStep], counter: &mut usize) {
    for step in steps {
        if let WorkflowStep::Route {
            id,
            branches,
            default,
            ..
        } = step
        {
            *id = format!("branch_{counter}");
            *counter += 1;
            for (_key, branch_steps) in branches {
                renumber_branches_inner(branch_steps, counter);
            }
            if let Some(default_steps) = default {
                renumber_branches_inner(default_steps, counter);
            }
        }
    }
}
