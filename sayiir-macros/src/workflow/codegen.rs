use proc_macro2::TokenStream;
use quote::quote;

use super::ast::WorkflowStep;
use crate::util::err;

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
                                .branch_registered(#struct_name::task_id())?
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
                            )?
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
                    )?
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
        }
    }

    Ok(tokens)
}
