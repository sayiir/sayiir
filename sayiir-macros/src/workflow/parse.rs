use proc_macro2::Span;
use syn::parse::{Parse, ParseStream};
use syn::token::Paren;
use syn::{Expr, Ident, LitStr, Token, Type, braced, parenthesized};

use crate::task::duration::DurationLit;
use crate::util::{err, snake_to_pascal};

use super::ast::{BranchArmKey, WorkflowDef, WorkflowStep};

/// Parse a `[step, step, ...]` bracketed pipeline used inside `route` arms.
fn parse_bracketed_pipeline(input: ParseStream) -> syn::Result<Vec<WorkflowStep>> {
    let content;
    syn::bracketed!(content in input);
    parse_comma_separated_steps(&content)
}

/// Parse comma-separated steps inside `[...]`.
fn parse_comma_separated_steps(input: ParseStream) -> syn::Result<Vec<WorkflowStep>> {
    let mut steps = Vec::new();

    if input.is_empty() {
        return Err(input.error("expected at least one pipeline step"));
    }

    steps.push(parse_step_or_parallel(input)?);

    while !input.is_empty() {
        // Require a comma between steps (trailing comma OK)
        if input.peek(Token![,]) {
            input.parse::<Token![,]>()?;
            if input.is_empty() {
                break; // trailing comma
            }
            steps.push(parse_step_or_parallel(input)?);
        } else {
            break;
        }
    }

    Ok(steps)
}

impl Parse for WorkflowDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut id: Option<LitStr> = None;
        let mut codec: Option<syn::Path> = None;
        let mut registry: Option<Expr> = None;
        let mut metadata: Option<Expr> = None;
        let mut steps: Option<Vec<WorkflowStep>> = None;

        // Parse named fields in any order: name: ..., codec: ..., [registry: ...,] steps: [...]
        while !input.is_empty() {
            let field: Ident = input.parse()?;
            input.parse::<Token![:]>()?;

            match field.to_string().as_str() {
                "name" => {
                    if id.is_some() {
                        return Err(err(field.span(), "duplicate `name` field"));
                    }
                    id = Some(input.parse()?);
                }
                "codec" => {
                    if codec.is_some() {
                        return Err(err(field.span(), "duplicate `codec` field"));
                    }
                    codec = Some(input.parse()?);
                }
                "registry" => {
                    if registry.is_some() {
                        return Err(err(field.span(), "duplicate `registry` field"));
                    }
                    registry = Some(input.parse()?);
                }
                "metadata" => {
                    if metadata.is_some() {
                        return Err(err(field.span(), "duplicate `metadata` field"));
                    }
                    metadata = Some(input.parse()?);
                }
                "steps" => {
                    if steps.is_some() {
                        return Err(err(field.span(), "duplicate `steps` field"));
                    }
                    let content;
                    syn::bracketed!(content in input);
                    let mut parsed = parse_comma_separated_steps(&content)?;
                    super::ast::renumber_branches(&mut parsed);
                    steps = Some(parsed);
                }
                other => {
                    return Err(err(
                        field.span(),
                        format!(
                            "unknown field `{other}`; expected `name`, `steps`, `codec`, `registry`, or `metadata`"
                        ),
                    ));
                }
            }

            // Optional trailing comma between fields
            if input.peek(Token![,]) {
                input.parse::<Token![,]>()?;
            }
        }

        let id = id.ok_or_else(|| input.error("missing required field `name`"))?;
        // Default codec to JsonCodec when omitted.
        let codec = codec.unwrap_or_else(|| {
            syn::parse_str("::sayiir_runtime::serialization::JsonCodec")
                .expect("hardcoded path is valid")
        });
        let steps = steps.ok_or_else(|| input.error("missing required field `steps`"))?;

        Ok(WorkflowDef {
            id,
            codec,
            registry,
            metadata,
            steps,
        })
    }
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
    // route classify { ... }  (Ident Ident Brace — check via fork)
    // flow child_expr          (Ident Expr — check via fork)
    if input.peek(Ident) {
        let fork = input.fork();
        let ident: Ident = fork.parse()?;
        if ident == "route" {
            // Consume from real stream
            let _: Ident = input.parse()?;
            return parse_route(input, ident.span());
        }
        if ident == "flow" {
            // Consume from real stream
            let _: Ident = input.parse()?;
            let expr: Expr = input.parse()?;
            return Ok(WorkflowStep::Flow {
                expr,
                span: ident.span(),
            });
        }
    }

    // loop refine_task 10  OR  loop refine_task 10 exit_with_last
    // `loop` is a Rust keyword — peek with Token![loop], not Ident.
    if input.peek(Token![loop]) {
        let kw: Token![loop] = input.parse()?;
        return parse_loop(input, kw.span);
    }

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
        // Not "delay" or "signal" — but we consumed the ident
        return Err(err(
            ident.span(),
            format!(
                "unexpected identifier `{ident}` followed by string literal; did you mean `delay`, `signal`, or `route`?"
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
        let pascal = format!("{}Task", snake_to_pascal(&ident.to_string()));
        let struct_name = Ident::new(&pascal, ident.span());
        return Ok(WorkflowStep::TaskRef {
            span: ident.span(),
            struct_name,
        });
    }

    Err(input.error(
        "expected a task name, inline task, `delay`, `signal`, `route`, `loop`, `flow`, or `(`",
    ))
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

/// Parse `loop body_task max_iterations` or `loop body_task max_iterations exit_with_last`.
///
/// The `loop` keyword has already been consumed.
fn parse_loop(input: ParseStream, span: Span) -> syn::Result<WorkflowStep> {
    // Body task identifier (e.g. `refine`)
    let body_ident: Ident = input.parse().map_err(|_| {
        err(
            span,
            "expected a task name after `loop`, e.g. loop refine_task 10",
        )
    })?;
    let body = Ident::new(
        &format!("{}Task", snake_to_pascal(&body_ident.to_string())),
        body_ident.span(),
    );

    // Max iterations (integer literal)
    let max_iterations: syn::LitInt = input.parse().map_err(|_| {
        err(
            span,
            "expected max_iterations integer after task name, e.g. loop refine_task 10",
        )
    })?;

    // Optional on_max policy: `exit_with_last`
    let on_max = if input.peek(Ident) {
        let kw: Ident = input.parse()?;
        if kw == "exit_with_last" {
            Some(kw)
        } else {
            return Err(err(
                kw.span(),
                "expected `exit_with_last` after max_iterations, \
                 or `,` / `]` to end the loop step",
            ));
        }
    } else {
        None
    };

    Ok(WorkflowStep::Loop {
        body,
        max_iterations,
        on_max,
        span,
    })
}

/// Parse `route key_fn { "key" => [pipeline], ... }` (string keys)
/// or    `route key_fn -> Type { Variant => [pipeline], ... }` (typed keys).
///
/// The `route` keyword has already been consumed.
fn parse_route(input: ParseStream, span: Span) -> syn::Result<WorkflowStep> {
    // Key function identifier (e.g. `classify`)
    let key_fn_ident: Ident = input.parse().map_err(|_| {
        err(
            span,
            "expected a key function name after `route`, e.g. route classify { ... }",
        )
    })?;
    // ID is a placeholder; renumber_branches() assigns the final positional ID.
    let id = String::new();
    let key_fn = Ident::new(
        &format!("{}Task", snake_to_pascal(&key_fn_ident.to_string())),
        key_fn_ident.span(),
    );

    // Optional type annotation: `-> Type`
    let key_type: Option<syn::Path> = if input.peek(Token![->]) {
        input.parse::<Token![->]>()?;
        Some(input.parse()?)
    } else {
        None
    };

    // Braced body: { key => [steps], _ => [steps] }
    let brace_content;
    braced!(brace_content in input);

    let mut branches: Vec<(BranchArmKey, Vec<WorkflowStep>)> = Vec::new();
    let mut default: Option<Vec<WorkflowStep>> = None;

    while !brace_content.is_empty() {
        if brace_content.peek(Token![_]) {
            // Default arm: _ => [pipeline]
            brace_content.parse::<Token![_]>()?;
            brace_content.parse::<Token![=>]>()?;
            let steps = parse_bracketed_pipeline(&brace_content)?;
            if default.is_some() {
                return Err(err(span, "duplicate default branch `_`"));
            }
            default = Some(steps);
        } else if key_type.is_some() && brace_content.peek(Ident) {
            // Typed variant arm: Variant => [pipeline]
            let variant: Ident = brace_content.parse()?;
            brace_content.parse::<Token![=>]>()?;
            let steps = parse_bracketed_pipeline(&brace_content)?;
            branches.push((BranchArmKey::Variant(variant), steps));
        } else if brace_content.peek(LitStr) {
            // String literal arm: "key" => [pipeline]
            let key: LitStr = brace_content.parse()?;
            brace_content.parse::<Token![=>]>()?;
            let steps = parse_bracketed_pipeline(&brace_content)?;
            branches.push((BranchArmKey::Literal(key), steps));
        } else {
            let msg = if key_type.is_some() {
                "expected a variant name, string literal, or `_` for default branch"
            } else {
                "expected a string literal key or `_` for default branch"
            };
            return Err(brace_content.error(msg));
        }

        // Optional trailing comma
        if brace_content.peek(Token![,]) {
            brace_content.parse::<Token![,]>()?;
        }
    }

    if branches.is_empty() {
        return Err(err(span, "route must have at least one named branch"));
    }

    Ok(WorkflowStep::Route {
        id,
        key_fn,
        key_type,
        branches,
        default,
        span,
    })
}

impl WorkflowStep {
    pub fn span(&self) -> Span {
        match self {
            Self::TaskRef { span, .. }
            | Self::InlineTask { span, .. }
            | Self::Parallel { span, .. }
            | Self::Delay { span, .. }
            | Self::AwaitSignal { span, .. }
            | Self::Loop { span, .. }
            | Self::Flow { span, .. }
            | Self::Route { span, .. } => *span,
        }
    }
}
