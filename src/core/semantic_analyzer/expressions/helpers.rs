// src/core/semantic_analyzer/expressions/helpers.rs
//! Semantic analysis for helper expressions like `convert`, `parse`, and `allow`.
//! This module has been updated to use the error-collecting paradigm.

use crate::core::ast::typed::*;
use crate::core::ast::untyped;
use crate::core::semantic_analyzer::{
    types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer,
};
use crate::diagnostics::errors::SemanticError;
use std::path::PathBuf;

impl SemanticAnalyzer<'_> {
    /// Analyzes an `allow` expression. It sets a context flag to true, allowing
    /// sub-expressions to perform unsafe operations like numeric demotion, raw pointer
    /// dereferencing, or calling unsafe functions.
    pub fn analyze_allow_expr(
        &mut self,
        inner_expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        // Set the flag indicating we are inside an `allow` block.
        let original_in_allow = ctx.in_allow_block;
        ctx.in_allow_block = true;

        // Analyze the inner expression with the `in_allow_block` flag set.
        // This is where special logic in other analysis functions will trigger.
        let typed_inner = self.analyze_expression(inner_expr, ctx, None, errors);

        // Restore the original state of the flag after we are done.
        ctx.in_allow_block = original_in_allow;
        
        if typed_inner.jophet_type == JophetType::ErrorSentinel {
            // If the inner expression failed to analyze, propagate the sentinel.
            return Ok(typed_inner);
        }

        // The `allow` keyword itself doesn't change the type of the expression.
        // It just creates a typed AST node to signify the context.
        return Ok(TypedExpression {
            kind: TypedExpressionKind::Allow(Box::new(typed_inner.clone())),
            jophet_type: typed_inner.jophet_type,
            span,
        });
    }

    /// Analyzes an explicit `convert(expr, TargetType)` expression.
    pub fn analyze_convert_expr(
        &mut self,
        expr: &untyped::Expression,
        untyped_target_type: &untyped::Type,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        _allow_demotion_legacy: bool, // This parameter is now controlled by ctx.in_allow_block
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let target_type = self.resolve_type(untyped_target_type, false, None, ctx, span.clone())?;

        // Handle Python FFI conversion separately
        let typed_expr_for_py = self.analyze_expression(expr, ctx, Some(&JophetType::PythonObject{ brand: Box::new(self.resolve_type(&untyped::Type::Simple("PyAny".to_string()), false, None, ctx, span.clone()).unwrap())}), errors);
        if let JophetType::PythonObject { .. } = typed_expr_for_py.jophet_type {
            // Case 1: Re-branding a PythonObject
            if let JophetType::PythonObject { .. } = &target_type {
                 return Ok(TypedExpression {
                    kind: TypedExpressionKind::Convert {
                        expr: Box::new(typed_expr_for_py),
                        target_type: target_type.clone(),
                    },
                    jophet_type: target_type,
                    span,
                });
            }

             // Case 2: Extracting a native type from a PythonObject
            let result_type = JophetType::Fallible {
                ok: Box::new(target_type.clone()),
                err: Box::new(JophetType::Error {
                    name: "FfiError".to_string(),
                    module_path: PathBuf::from("std"),
                }),
            };
            return Ok(TypedExpression {
                kind: TypedExpressionKind::Convert {
                    expr: Box::new(typed_expr_for_py),
                    target_type,
                },
                jophet_type: result_type,
                span,
            });
        }
        
        let typed_expr = self.analyze_expression(expr, ctx, None, errors);
        if typed_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_expr); }

        if !self.is_valid_conversion(&typed_expr.jophet_type, &target_type, ctx.in_allow_block) {
            let mut message = format!(
                "Cannot convert from '{}' to '{}'",
                jophet_type_to_user_string(&typed_expr.jophet_type), jophet_type_to_user_string(&target_type)
            );
            if !ctx.in_allow_block {
                let is_unsafe_cast = matches!((&typed_expr.jophet_type, &target_type), (JophetType::RawPointer(_), _) | (_, JophetType::RawPointer(_)) | (JophetType::Array{..}, JophetType::RawPointer(_)));
                if is_unsafe_cast {
                     message.push_str(". This is an unsafe conversion. Use `allow convert(...)` to perform this operation.");
                } else {
                     message.push_str(". This is a demotion, which may lose data. Use `allow convert(...)` to perform this conversion.");
                }
            }
            return Err(SemanticError::TypeError {
                message,
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        Ok(TypedExpression {
            kind: TypedExpressionKind::Convert {
                expr: Box::new(typed_expr),
                target_type: target_type.clone(),
            },
            jophet_type: target_type,
            span,
        })
    }

    /// Analyzes a `parse(Type, String)` expression.
    pub fn analyze_parse_expr(
        &mut self,
        untyped_target_type: &untyped::Type,
        parse_expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let target_type = self.resolve_type(untyped_target_type, false, None, ctx, span.clone())?;

        // 1. Check if the target type is a valid, parseable numeric type.
        if !matches!(
            target_type,
            JophetType::Int(_) | JophetType::UInt(_) | JophetType::Float(_)
        ) {
            return Err(SemanticError::TypeError {
                message: format!(
                    "The first argument to `parse` must be a numeric type (e.g., Int64, Float64), but found '{}'",
                    jophet_type_to_user_string(&target_type)
                ),
                span, // Consider a more specific span for the type argument
                file_path: self.current_module_path.clone(),
            });
        }

        // 2. Analyze the expression to be parsed, ensuring it's a string type.
        let typed_expr = self.analyze_expression(
            parse_expr,
            ctx,
            Some(&JophetType::String),
            errors,
        );
        if typed_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_expr); }

        if !matches!(
            typed_expr.jophet_type,
            JophetType::String | JophetType::StringSlice
        ) {
            return Err(SemanticError::TypeError {
                message: format!(
                    "The second argument to `parse` must be a String or StringSlice, but found '{}'",
                    jophet_type_to_user_string(&typed_expr.jophet_type)
                ),
                span: parse_expr.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        // 3. The result of `parse` is always fallible, returning a structured ParseError.
        let result_type = JophetType::Fallible {
            ok: Box::new(target_type.clone()),
            err: Box::new(JophetType::Error {
                name: "ParseError".to_string(),
                module_path: PathBuf::from("std"),
            }),
        };

        Ok(TypedExpression {
            kind: TypedExpressionKind::Parse {
                target_type,
                expr: Box::new(typed_expr),
            },
            jophet_type: result_type,
            span,
        })
    }
}