use proc_macro2::Span;
use syn::{Expr, Ident, Type};

use crate::task::duration::DurationLit;

/// Top-level workflow definition parsed from `workflow!(...)`.
#[derive(Debug)]
pub struct WorkflowDef {
    /// Workflow ID string literal.
    pub id: syn::LitStr,
    /// Codec type path.
    pub codec: syn::Path,
    /// Registry expression.
    pub registry: Expr,
    /// Pipeline steps.
    pub steps: Vec<WorkflowStep>,
}

/// A single step in the workflow pipeline.
#[derive(Debug)]
pub enum WorkflowStep {
    /// Reference to a `#[task]`-generated struct by function name.
    /// e.g. `charge` → resolves to `Charge`
    TaskRef {
        #[allow(dead_code)]
        ident: Ident,
        struct_name: Ident,
        span: Span,
    },

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
}
