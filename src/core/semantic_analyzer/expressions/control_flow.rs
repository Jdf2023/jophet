// src/core/semantic_analyzer/expressions/control_flow.rs
//! Semantic analysis for expressions that involve control flow.
//!
//! This module handles the analysis of `switch`, `try`, and `catch` expressions,
//! ensuring type safety, exhaustiveness, and correct error handling logic.
//! It has been updated to use the error-collecting paradigm.

use crate::core::ast::typed::*;
use crate::core::ast::untyped;
use crate::core::ast::Literal;
use crate::core::semantic_analyzer::{
    context, types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer,
};
use crate::diagnostics::errors::SemanticError;
use std::collections::HashSet;

impl SemanticAnalyzer<'_> {
    /// Analyzes a `switch` expression, ensuring type consistency across patterns and checking
    /// for exhaustiveness. It now supports destructuring patterns for tagged unions and errors.
    /// If the switch is used as an expression, it now enforces that every branch must yield a value.
    pub fn analyze_switch_expression(
        &mut self,
        expression: &untyped::Expression,
        cases: &[untyped::SwitchCase],
        else_block: &Option<Vec<untyped::Statement>>,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        is_expression_context: bool,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_expression = self.analyze_expression(expression, ctx, None, errors);
        if typed_expression.jophet_type == JophetType::ErrorSentinel {
            return Ok(typed_expression);
        }
        let switch_on_type = typed_expression.jophet_type.clone();

        let (switch_type_def, is_error_type) = match &switch_on_type {
            JophetType::TaggedUnion { name, .. } => (self.tagged_union_defs.get(name).cloned(), false),
            JophetType::Error { name, .. } => (self.error_defs.get(name).cloned().map(|e| {
                untyped::TaggedUnionDef {
                    is_public: e.is_public, name: e.name.clone(), doc_comment: e.doc_comment.clone(),
                    generic_params: vec![], variants: e.variants.clone(), module_path: e.module_path.clone(),
                }
            }), true),
            _ => (None, false)
        };

        let mut typed_cases = Vec::new();
        let mut covered_variants = HashSet::new();

        let outer_yield_type = ctx.current_switch_yield_type.clone();
        ctx.current_switch_yield_type = Some(JophetType::Nothing);

        for case in cases {
            let mut typed_patterns = Vec::new();
            for pattern in &case.patterns {
                match pattern {
                    untyped::Pattern::Literal(expr) => {
                        let typed_pattern = self.analyze_expression(expr, ctx, Some(&switch_on_type), errors);
                        if typed_pattern.jophet_type == JophetType::ErrorSentinel { return Ok(typed_pattern); }

                        if typed_pattern.jophet_type != switch_on_type {
                            return Err(SemanticError::TypeError {
                                message: format!(
                                    "Type mismatch in switch case pattern. Expected {:?}, but pattern has type {:?}",
                                    switch_on_type, typed_pattern.jophet_type
                                ),
                                span: expr.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                        match &typed_pattern.kind {
                            // This now correctly handles both C-style enums AND payload-less
                            // tagged unions/errors, because they both resolve to this AST kind.
                            TypedExpressionKind::EnumVariantAccess {
                                variant_name, ..
                            } => {
                                if matches!(&switch_on_type, JophetType::Enum { .. }) {
                                    covered_variants.insert(variant_name.clone());
                                }
                            }
                            TypedExpressionKind::TaggedUnionInstantiation { variant_name, payload, .. } if payload.is_none() => {
                                if matches!(&switch_on_type, JophetType::TaggedUnion { .. } | JophetType::Error { .. }) {
                                     covered_variants.insert(variant_name.clone());
                                }
                            }
                            TypedExpressionKind::Literal(Literal::Bool(b)) => {
                                covered_variants.insert(b.to_string());
                            }
                            _ => {}
                        }
                        typed_patterns.push(TypedPattern::Literal(typed_pattern));
                    }
                    untyped::Pattern::Destructure { variant_path, binding, span } => {
                        let (enum_name, variant_name) = variant_path;
                        let def = switch_type_def.as_ref().ok_or_else(|| SemanticError::TypeError {
                            message: format!("Cannot destructure type '{}' as it is not a tagged union or error type.", jophet_type_to_user_string(&switch_on_type)),
                            span: span.clone(),
                            file_path: self.current_module_path.clone(),
                        })?;

                        if &def.name != enum_name {
                             return Err(SemanticError::TypeError {
                                message: format!("This switch is on type '{}', but the pattern is for '{}'.", def.name, enum_name),
                                span: span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }

                        let variant_def = def.variants.iter().find(|v| &v.name == variant_name).ok_or_else(|| SemanticError::NameError {
                            message: format!("Type '{}' has no variant named '{}'", def.name, variant_name),
                            span: span.clone(),
                            file_path: self.current_module_path.clone(),
                        })?;

                        let mut typed_binding = None;
                        match (&variant_def.payload, binding) {
                            (Some(untyped_payload), Some(var_name)) => {
                                let payload_type = self.resolve_type(untyped_payload, is_error_type, Some(&def.name), ctx, span.clone())?;
                                typed_binding = Some((var_name.clone(), payload_type));
                            },
                            (None, None) => {},
                            (Some(_), None) => {
                                return Err(SemanticError::TypeError {
                                    message: format!("Variant '{}.{}' expects a payload, but none was provided in the pattern. Use '{}.{}(value)' instead.", def.name, variant_name, def.name, variant_name),
                                    span: span.clone(),
                                    file_path: self.current_module_path.clone(),
                                });
                            },
                            (None, Some(_)) => {
                                 return Err(SemanticError::TypeError {
                                    message: format!("Variant '{}.{}' does not have a payload, so it cannot be destructured with a variable.", def.name, variant_name),
                                    span: span.clone(),
                                    file_path: self.current_module_path.clone(),
                                });
                            }
                        }
                        
                        covered_variants.insert(variant_name.clone());

                        typed_patterns.push(TypedPattern::Destructure {
                            enum_type: switch_on_type.clone(),
                            variant_name: variant_name.clone(),
                            binding: typed_binding,
                        });
                    }
                }
            }

            let mut case_body_ctx = ctx.clone();
            if let Some(TypedPattern::Destructure { binding: Some((name, ty)), .. }) = typed_patterns.first() {
                case_body_ctx.symbol_table.insert(name.clone(), context::SymbolInfo {
                    jophet_type: ty.clone(),
                    is_mutable: false,
                    is_const: false,
                    mangled_name: None,
                });
            }

            let (body, did_yield) = self.analyze_block(&case.body, &mut case_body_ctx, None, false, errors);

            if is_expression_context && !did_yield {
                return Err(SemanticError::FlowError {
                    message: "This branch of the switch expression must yield a value.".to_string(),
                    span: case.patterns.first().unwrap().span().clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            typed_cases.push(TypedSwitchCase {
                patterns: typed_patterns,
                body,
            });
        }

        let typed_else_block = if let Some(else_stmts) = else_block {
            let (body, did_yield) = self.analyze_block(else_stmts, ctx, None, false, errors);
            if is_expression_context && !did_yield {
                return Err(SemanticError::FlowError {
                    message: "The 'else' branch of this switch expression must yield a value."
                        .to_string(),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
            Some(body)
        } else {
            None
        };

        if typed_else_block.is_none() {
            let is_exhaustive = match &switch_on_type {
                JophetType::Enum { members, .. } => covered_variants.len() == members.len(),
                JophetType::Bool => {
                    covered_variants.contains("true") && covered_variants.contains("false")
                }
                JophetType::TaggedUnion { name, .. } => {
                    let def = self.tagged_union_defs.get(name).unwrap();
                    let all_variants: HashSet<String> =
                        def.variants.iter().map(|v| v.name.clone()).collect();
                    covered_variants == all_variants
                }
                JophetType::Error { name, .. } => {
                    let def = self.error_defs.get(name).unwrap();
                    let all_variants: HashSet<String> =
                        def.variants.iter().map(|v| v.name.clone()).collect();
                    covered_variants == all_variants
                }
                _ => false,
            };

            if !is_exhaustive {
                return Err(SemanticError::FlowError {
                    message: "Non-exhaustive switch requires an 'else' block.".to_string(),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        let final_yield_type = ctx.current_switch_yield_type.clone().unwrap();
        ctx.current_switch_yield_type = outer_yield_type;

        Ok(TypedExpression {
            kind: TypedExpressionKind::Switch {
                expression: Box::new(typed_expression),
                cases: typed_cases,
                else_block: typed_else_block,
            },
            jophet_type: final_yield_type,
            span,
        })
    }

    /// Analyzes a `try` expression. Its behavior depends on the context:
    ///
    /// 1.  **In a fallible function**: It acts as an error propagation operator.
    ///     - It must be used on a `Fallible` type.
    ///     - The error type of the expression must be compatible with the function's return error type.
    ///     - If these conditions are met, it produces a `PropagateError` node in the AST.
    /// 2.  **In a non-fallible function or the global scope**: It acts as an "unwrap or panic" operator.
    ///     - It must be used on a `Fallible` type.
    ///     - If the expression results in an error, the program will terminate.
    ///     - If it succeeds, the expression evaluates to the unwrapped `Ok` value.
    ///     - This produces an `UnwrapOrPanic` node in the AST.
    pub fn analyze_try_expr(
        &mut self,
        try_expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_inner_expr = self.analyze_expression(try_expr, ctx, None, errors);
        if typed_inner_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_inner_expr); }

        let ok_type = if let JophetType::Fallible { ok, .. } = &typed_inner_expr.jophet_type {
            ok.as_ref().clone()
        } else {
            return Err(SemanticError::TypeError {
                message: format!(
                    "'try' can only be used on a fallible type (e.g., Type?), but found '{}'.",
                    jophet_type_to_user_string(&typed_inner_expr.jophet_type)
                ),
                span: try_expr.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        };

        // Check if we are in a fallible context (a function that returns a Fallible type).
        let is_fallible_context = if let Some(func_ret_type) = &ctx.current_function_return_type {
            matches!(func_ret_type, JophetType::Fallible { .. })
        } else {
            false // Global scope is not a fallible context.
        };

        if is_fallible_context {
            // --- BEHAVIOR 1: PROPAGATE ERROR ---
            let func_ret_type = ctx.current_function_return_type.as_ref().unwrap();
            let (inner_err_type, func_err_type) =
                if let (JophetType::Fallible { err: inner_err, .. }, JophetType::Fallible { err: func_err, .. }) =
                    (&typed_inner_expr.jophet_type, func_ret_type)
                {
                    (inner_err.as_ref(), func_err.as_ref())
                } else {
                    unreachable!()
                };

            if !self.is_type_compatible(inner_err_type, func_err_type) {
                return Err(SemanticError::TypeError {
                   message: format!(
                       "The error type '{}' from this operation cannot be converted to the function's error return type '{}'.",
                       jophet_type_to_user_string(inner_err_type),
                       jophet_type_to_user_string(func_err_type)
                   ),
                   span,
                   file_path: self.current_module_path.clone(),
               });
            }

            Ok(TypedExpression {
                kind: TypedExpressionKind::PropagateError {
                    expr: Box::new(typed_inner_expr),
                },
                jophet_type: ok_type,
                span,
            })
        } else {
            // --- BEHAVIOR 2: UNWRAP OR PANIC ---
            Ok(TypedExpression {
                kind: TypedExpressionKind::UnwrapOrPanic {
                    expr: Box::new(typed_inner_expr),
                },
                jophet_type: ok_type,
                span,
            })
        }
    }

    /// Analyzes a `catch` expression, which provides a recovery path for a fallible operation.
    ///
    /// It creates a new scope for the `catch` block, making the error value available.
    /// It enforces that the `catch` block **must** explicitly `yield` a value compatible with the success
    /// (`Ok`) type of the fallible expression, ensuring that the entire `catch` expression
    /// always resolves to a single, non-error value.
    pub fn analyze_catch_expr(
        &mut self,
        expression: &untyped::Expression,
        error_variable: &str,
        body: &[untyped::Statement],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_expr = self.analyze_expression(expression, ctx, None, errors);
        if typed_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_expr); }

        // Step 1: Verify the expression is fallible and extract its Ok and Err types.
        let (ok_type, err_type) =
           if let JophetType::Fallible { ok, err } = &typed_expr.jophet_type {
               (ok.as_ref().clone(), err.as_ref().clone())
            } else {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "'catch' can only be used on a fallible type (Type?), but found '{}'.",
                        jophet_type_to_user_string(&typed_expr.jophet_type)
                    ),
                    span: expression.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            };

        // Step 2: Analyze the 'catch' block in a new scope where the error variable exists.
        let mut catch_ctx = ctx.clone();
        catch_ctx.symbol_table.insert(
            error_variable.to_string(),
            context::SymbolInfo {
                jophet_type: err_type,
                is_mutable: false,
                is_const: false,
                mangled_name: None,
            },
        );

        // Step 3: Crucially, set the expected yield type for the block to be the `Ok` type.
        let outer_yield_type = catch_ctx.current_switch_yield_type.clone();
        catch_ctx.current_switch_yield_type = Some(ok_type.clone());

        let (typed_body, did_yield) = self.analyze_block(body, &mut catch_ctx, None, false, errors);

        // Restore the outer context's yield type state.
        ctx.current_switch_yield_type = outer_yield_type;

        // Step 4: Enforce that the block MUST yield a value.
        if !did_yield {
            return Err(SemanticError::FlowError {
                message: "A 'catch' block must explicitly yield a value to provide a fallback for the expression.".to_string(),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        // Note: The check that the yielded type matches `ok_type` is now implicitly handled
        // inside `analyze_yield`, which uses `current_switch_yield_type`. No extra check is needed here.

        // Step 5: The final type of the entire expression is the `Ok` type, because `catch` guarantees recovery.
        Ok(TypedExpression {
            kind: TypedExpressionKind::Catch {
                expression: Box::new(typed_expr),
                error_variable: error_variable.to_string(),
                body: typed_body,
            },
            jophet_type: ok_type, // The type is no longer Fallible, it's the success type.
            span,
        })
    }
}