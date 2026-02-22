use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;

use super::ast::{BranchArmKey, WorkflowStep};
use crate::util::err;

/// Collect all `TaskRef` struct names from the pipeline (including inside forks and branches).
pub fn collect_task_refs(steps: &[WorkflowStep]) -> Vec<&Ident> {
    let mut refs = Vec::new();
    for step in steps {
        match step {
            WorkflowStep::TaskRef { struct_name, .. } => {
                refs.push(struct_name);
            }
            WorkflowStep::Parallel { branches, .. } => {
                refs.extend(collect_task_refs(branches));
            }
            WorkflowStep::Route {
                key_fn,
                branches,
                default,
                ..
            } => {
                refs.push(key_fn);
                for (_key, steps) in branches {
                    refs.extend(collect_task_refs(steps));
                }
                if let Some(default_steps) = default {
                    refs.extend(collect_task_refs(default_steps));
                }
            }
            WorkflowStep::Loop { body, .. } => {
                refs.push(body);
            }
            WorkflowStep::InlineTask { .. }
            | WorkflowStep::Delay { .. }
            | WorkflowStep::AwaitSignal { .. }
            | WorkflowStep::Flow { .. } => {}
        }
    }
    refs
}

/// Collect `route` key function aliases: `(key_fn_struct, alias_id)`.
///
/// Each `route classify_key { ... }` needs the key function
/// registered under `"branch_classify_key::key_fn"` in addition to its own task ID.
pub fn collect_route_aliases(steps: &[WorkflowStep]) -> Vec<(&Ident, String)> {
    let mut aliases = Vec::new();
    for step in steps {
        if let WorkflowStep::Route {
            id,
            key_fn,
            branches,
            default,
            ..
        } = step
        {
            // NB: must match `sayiir_core::workflow::key_fn_id` convention
            aliases.push((key_fn, format!("{id}::key_fn")));
            // Recurse into branch arms (they could contain nested route)
            for (_key, branch_steps) in branches {
                aliases.extend(collect_route_aliases(branch_steps));
            }
            if let Some(default_steps) = default {
                aliases.extend(collect_route_aliases(default_steps));
            }
        }
    }
    aliases
}

/// Generate the chained method calls, handling fork-join grouping.
pub fn gen_step_chain(steps: &[WorkflowStep]) -> syn::Result<TokenStream> {
    let mut tokens = TokenStream::new();
    let mut i = 0;

    while i < steps.len() {
        match &steps[i] {
            WorkflowStep::Parallel { branches, span } => {
                if branches.is_empty() {
                    return Err(err(*span, "parallel fork must have at least one branch"));
                }

                // Must be followed by a join step
                let join_idx = i + 1;
                if join_idx >= steps.len() {
                    return Err(err(
                        *span,
                        "parallel fork must be followed by a join step (e.g. `=> join_task`)",
                    ));
                }

                let join_step = &steps[join_idx];

                // Generate fork branches
                let mut fork_tokens = quote! { .fork() };
                for branch in branches {
                    match branch {
                        WorkflowStep::TaskRef { struct_name, .. } => {
                            fork_tokens.extend(quote! {
                                .branch_registered(#struct_name::task_id())
                            });
                        }
                        WorkflowStep::InlineTask { .. } => {
                            return Err(err(
                                branch.span(),
                                "inline tasks in parallel branches are not yet supported; register them first with #[task]",
                            ));
                        }
                        _ => {
                            return Err(err(
                                branch.span(),
                                "only task references are supported in parallel branches",
                            ));
                        }
                    }
                }

                // Generate join
                match join_step {
                    WorkflowStep::TaskRef { struct_name, .. } => {
                        fork_tokens.extend(quote! {
                            .join_registered::<<#struct_name as ::sayiir_core::task::CoreTask>::Output>(
                                #struct_name::task_id()
                            )
                        });
                    }
                    _ => {
                        return Err(err(
                            join_step.span(),
                            "join step after parallel fork must be a task reference",
                        ));
                    }
                }

                tokens.extend(fork_tokens);
                i += 2; // Skip both parallel and join
            }
            WorkflowStep::TaskRef { struct_name, .. } => {
                tokens.extend(quote! {
                    .then_registered::<<#struct_name as ::sayiir_core::task::CoreTask>::Output>(
                        #struct_name::task_id()
                    )
                });
                i += 1;
            }
            WorkflowStep::InlineTask {
                name,
                param_name,
                param_type,
                body,
                ..
            } => {
                let id_str = name.to_string();
                tokens.extend(quote! {
                    .then(#id_str, |#param_name: #param_type| async move {
                        #body
                    })
                });
                i += 1;
            }
            WorkflowStep::Delay { id, duration, .. } => {
                let dur = duration.to_tokens();
                tokens.extend(quote! {
                    .delay(#id, #dur)
                });
                i += 1;
            }
            WorkflowStep::AwaitSignal {
                id,
                signal_name,
                timeout,
                ..
            } => {
                let timeout_expr = match timeout {
                    Some(dur) => {
                        let dur_tokens = dur.to_tokens();
                        quote! { ::core::option::Option::Some(#dur_tokens) }
                    }
                    None => quote! { ::core::option::Option::None },
                };
                tokens.extend(quote! {
                    .wait_for_signal(#id, #signal_name, #timeout_expr)
                });
                i += 1;
            }
            WorkflowStep::Loop {
                body,
                max_iterations,
                on_max,
                ..
            } => {
                let on_max_expr = match on_max {
                    Some(ident) if ident == "exit_with_last" => quote! {
                        ::sayiir_core::MaxIterationsPolicy::ExitWithLast
                    },
                    _ => quote! {
                        ::sayiir_core::MaxIterationsPolicy::Fail
                    },
                };
                tokens.extend(quote! {
                    .loop_task_registered::<<<#body as ::sayiir_core::task::CoreTask>::Output as ::sayiir_core::loop_result::LoopOutput>::Inner>(
                        #body::task_id(),
                        #max_iterations,
                        #on_max_expr,
                    )
                });
                i += 1;
            }
            WorkflowStep::Flow { expr, .. } => {
                tokens.extend(quote! {
                    .then_serializable_flow(#expr)
                });
                i += 1;
            }
            WorkflowStep::Route {
                id,
                key_fn: _,
                key_type,
                branches,
                default,
                span,
            } => {
                // Determine the output type from the last task in any branch.
                let output_type = infer_branch_output_type(branches, default.as_deref(), *span)?;

                tokens.extend(quote! {
                    .route_registered::<#output_type>(#id)
                });

                // Generate each named branch
                for (key, branch_steps) in branches {
                    let task_ids = collect_branch_task_ids(branch_steps, *span)?;
                    let task_id_exprs: Vec<_> = task_ids
                        .iter()
                        .map(|name| quote! { #name::task_id() })
                        .collect();
                    let key_expr = match key {
                        BranchArmKey::Literal(lit) => quote! { #lit },
                        BranchArmKey::Variant(variant) => {
                            let ty = key_type
                                .as_ref()
                                .expect("BranchArmKey::Variant requires key_type");
                            quote! {
                                <#ty as ::sayiir_core::branch_key::BranchKey>::as_key(&#ty::#variant)
                            }
                        }
                    };
                    tokens.extend(quote! {
                        .branch_registered(#key_expr, &[#(#task_id_exprs),*])
                    });
                }

                // Generate default branch if present
                if let Some(default_steps) = default {
                    let task_ids = collect_branch_task_ids(default_steps, *span)?;
                    let task_id_exprs: Vec<_> = task_ids
                        .iter()
                        .map(|name| quote! { #name::task_id() })
                        .collect();
                    tokens.extend(quote! {
                        .default_registered(&[#(#task_id_exprs),*])
                    });
                }

                tokens.extend(quote! { .done() });

                i += 1;
            }
        }
    }

    Ok(tokens)
}

/// Collect task struct names from a branch pipeline (only `TaskRef` supported).
fn collect_branch_task_ids(steps: &[WorkflowStep], span: Span) -> syn::Result<Vec<&Ident>> {
    let mut ids = Vec::new();
    for step in steps {
        match step {
            WorkflowStep::TaskRef { struct_name, .. } => {
                ids.push(struct_name);
            }
            _ => {
                return Err(err(
                    span,
                    "only task references are supported inside route arms; use #[task] to register complex steps",
                ));
            }
        }
    }
    if ids.is_empty() {
        return Err(err(span, "each route arm must have at least one task"));
    }
    Ok(ids)
}

/// Infer the output type of a `route` node by looking at the last task
/// in the first named branch. All branches must produce the same type.
fn infer_branch_output_type(
    branches: &[(BranchArmKey, Vec<WorkflowStep>)],
    _default: Option<&[WorkflowStep]>,
    span: Span,
) -> syn::Result<proc_macro2::TokenStream> {
    let first_branch = branches
        .first()
        .ok_or_else(|| err(span, "empty branches"))?;
    let last_step = first_branch
        .1
        .last()
        .ok_or_else(|| err(span, "branch must have at least one task"))?;
    match last_step {
        WorkflowStep::TaskRef { struct_name, .. } => Ok(quote! {
            <#struct_name as ::sayiir_core::task::CoreTask>::Output
        }),
        _ => Err(err(
            span,
            "route arms must end with a task reference to infer the output type",
        )),
    }
}

/// Generate compile-time exhaustiveness assertions for typed `route` nodes.
///
/// For each `route key_fn -> Type { Variant => [...], ... }`, emits a const
/// block containing a match on the enum type. The Rust compiler then verifies
/// that every variant is covered (unless a default `_` arm is present).
pub fn collect_exhaustiveness_checks(steps: &[WorkflowStep]) -> Vec<TokenStream> {
    let mut checks = Vec::new();
    collect_exhaustiveness_checks_inner(steps, &mut checks);
    checks
}

fn collect_exhaustiveness_checks_inner(steps: &[WorkflowStep], checks: &mut Vec<TokenStream>) {
    for step in steps {
        if let WorkflowStep::Route {
            key_type: Some(ty),
            branches,
            default,
            ..
        } = step
        {
            let match_arms: Vec<_> = branches
                .iter()
                .filter_map(|(key, _)| match key {
                    BranchArmKey::Variant(variant) => Some(quote! { #ty::#variant => {} }),
                    BranchArmKey::Literal(_) => None,
                })
                .collect();

            if !match_arms.is_empty() {
                let wildcard = if default.is_some() {
                    quote! { _ => {} }
                } else {
                    quote! {}
                };

                checks.push(quote! {
                    const _: () = {
                        #[allow(dead_code, unreachable_patterns)]
                        fn _exhaustive_check(k: #ty) {
                            match k {
                                #(#match_arms,)*
                                #wildcard
                            }
                        }
                    };
                });
            }

            // Recurse into branch arms
            for (_key, branch_steps) in branches {
                collect_exhaustiveness_checks_inner(branch_steps, checks);
            }
            if let Some(default_steps) = default {
                collect_exhaustiveness_checks_inner(default_steps, checks);
            }
        }
    }
}
