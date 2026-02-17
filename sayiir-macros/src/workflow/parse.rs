use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::token::Paren;
use syn::{Expr, Ident, LitStr, Token, Type, braced, parenthesized};

use crate::task::duration::DurationLit;
use crate::util::{err, snake_to_pascal};

use super::ast::{WorkflowDef, WorkflowStep};

impl Parse for WorkflowDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // 1. Workflow ID (string literal)
        let id: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;

        // 2. Codec type path
        let codec: syn::Path = input.parse()?;
        input.parse::<Token![,]>()?;

        // 3. Registry expression
        let registry: Expr = input.parse()?;
        input.parse::<Token![,]>()?;

        // 4. Pipeline steps separated by =>
        let steps = parse_pipeline(input)?;

        Ok(WorkflowDef {
            id,
            codec,
            registry,
            steps,
        })
    }
}

/// Parse a pipeline: `step => step => step`
fn parse_pipeline(input: ParseStream) -> syn::Result<Vec<WorkflowStep>> {
    let mut steps = Vec::new();

    if input.is_empty() {
        return Err(input.error("expected at least one pipeline step"));
    }

    steps.push(parse_step_or_parallel(input)?);

    while input.peek(Token![=>]) {
        input.parse::<Token![=>]>()?;
        steps.push(parse_step_or_parallel(input)?);
    }

    Ok(steps)
}

/// Parse a step that might be parallel: `a || b || c`
fn parse_step_or_parallel(input: ParseStream) -> syn::Result<WorkflowStep> {
    // Check for parenthesized group
    if input.peek(Paren) {
        let content;
        parenthesized!(content in input);
        return parse_step_or_parallel(&content);
    }

    let first = parse_single_step(input)?;

    // Check for `||`
    if input.peek(Token![||]) {
        let span = first.span();
        let mut branches = vec![first];
        while input.peek(Token![||]) {
            input.parse::<Token![||]>()?;
            branches.push(parse_single_step(input)?);
        }
        // Validate: no nested parallels
        for b in &branches {
            if matches!(b, WorkflowStep::Parallel { .. }) {
                return Err(err(
                    b.span(),
                    "nested parallel forks are not supported; use WorkflowBuilder for complex fork patterns",
                ));
            }
        }
        Ok(WorkflowStep::Parallel { branches, span })
    } else {
        Ok(first)
    }
}

/// Parse a single step (not parallel).
fn parse_single_step(input: ParseStream) -> syn::Result<WorkflowStep> {
    // delay "5s"  OR  delay "my_id" "5s"
    // signal "approval"  OR  signal "my_id" "approval"  OR  signal "approval" timeout "30s"
    if input.peek(Ident) && input.peek2(LitStr) {
        let ident: Ident = input.parse()?;
        if ident == "delay" {
            let first_lit: LitStr = input.parse()?;

            // Check if there's a second string literal (custom ID + duration)
            if input.peek(LitStr) {
                let second_lit: LitStr = input.parse()?;
                let id = first_lit.value();
                let duration = DurationLit::parse(&second_lit.value(), second_lit.span())?;
                return Ok(WorkflowStep::Delay {
                    id,
                    duration,
                    span: ident.span(),
                });
            }

            // Single string literal — auto-generate ID from duration
            let duration = DurationLit::parse(&first_lit.value(), first_lit.span())?;
            let id = format!("delay_{}", duration.millis);
            return Ok(WorkflowStep::Delay {
                id,
                duration,
                span: ident.span(),
            });
        }
        if ident == "signal" {
            let first_lit: LitStr = input.parse()?;

            // Two forms: `signal "custom_id" "name"` or `signal "name"`
            let (id, signal_name) = if input.peek(LitStr) {
                let second_lit: LitStr = input.parse()?;
                (first_lit.value(), second_lit.value())
            } else {
                let name = first_lit.value();
                (format!("signal_{name}"), name)
            };

            let timeout = parse_optional_timeout(input)?;
            return Ok(WorkflowStep::AwaitSignal {
                id,
                signal_name,
                timeout,
                span: ident.span(),
            });
        }
        // Not "delay" or "signal", fall through — but we consumed the ident, so we need to handle it
        return Err(err(
            ident.span(),
            format!(
                "unexpected identifier `{ident}` followed by string literal; did you mean `delay` or `signal`?"
            ),
        ));
    }

    // inline task: name(param: Type) { body }
    if input.peek(Ident) && input.peek2(Paren) {
        let name: Ident = input.parse()?;
        let paren_content;
        parenthesized!(paren_content in input);
        let param_name: Ident = paren_content.parse()?;
        paren_content.parse::<Token![:]>()?;
        let param_type: Type = paren_content.parse()?;

        let brace_content;
        braced!(brace_content in input);
        let body: Expr = brace_content.parse()?;

        return Ok(WorkflowStep::InlineTask {
            span: name.span(),
            name,
            param_name,
            param_type: Box::new(param_type),
            body,
        });
    }

    // bare task ref: identifier
    if input.peek(Ident) {
        let ident: Ident = input.parse()?;
        let pascal = snake_to_pascal(&ident.to_string());
        let struct_name = Ident::new(&pascal, ident.span());
        return Ok(WorkflowStep::TaskRef {
            span: ident.span(),
            struct_name,
        });
    }

    Err(input.error("expected a task name, inline task, `delay`, `signal`, or `(`"))
}

/// Parse an optional `timeout "30s"` suffix.
fn parse_optional_timeout(input: ParseStream) -> syn::Result<Option<DurationLit>> {
    if input.peek(Ident) {
        let fork = input.fork();
        let kw: Ident = fork.parse()?;
        if kw == "timeout" {
            // Consume from real stream
            let _: Ident = input.parse()?;
            let lit: LitStr = input.parse()?;
            let dur = DurationLit::parse(&lit.value(), lit.span())?;
            return Ok(Some(dur));
        }
    }
    Ok(None)
}

impl WorkflowStep {
    pub fn span(&self) -> Span {
        match self {
            Self::TaskRef { span, .. }
            | Self::InlineTask { span, .. }
            | Self::Parallel { span, .. }
            | Self::Delay { span, .. }
            | Self::AwaitSignal { span, .. } => *span,
        }
    }
}
