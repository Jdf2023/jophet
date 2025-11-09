// src/core/semantic_analyzer/expressions/literals.rs
//! Semantic analysis for literal expressions and interpolated strings.
//!
//! This module handles the analysis of primitive literals (integers, floats, strings, etc.)
//! and interpolated strings, determining their types and constructing the
//! appropriate typed AST nodes. It has been updated to use the error-collecting paradigm.

use crate::core::ast::typed::*;
use crate::core::ast::{untyped, Literal};
use crate::core::semantic_analyzer::{
    types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer,
};
use crate::diagnostics::errors::SemanticError;

impl SemanticAnalyzer<'_> {
    /// Analyzes a literal expression, determining its type based on a contextual hint or a default.
    pub fn analyze_literal_expr(
        &self,
        lit: &Literal,
        span: crate::core::ast::Span,
        expected_type: Option<&JophetType>,
    ) -> Result<TypedExpression, SemanticError> {
        let jophet_type = match lit {
            Literal::Int(i) => {
                if let Some(expected) = expected_type {
                    match expected {
                        JophetType::Int(bits) => {
                            let min = -(1i128 << (bits - 1));
                            let max = (1i128 << (bits - 1)) - 1;
                            if (*i as i128) >= min && (*i as i128) <= max {
                                JophetType::Int(*bits)
                            } else {
                                return Err(SemanticError::TypeError {
                                    message: format!(
                                        "Integer literal {} is out of range for type Int{}",
                                        i, bits
                                    ),
                                    span,
                                    file_path: self.current_module_path.clone(),
                                });
                            }
                        }
                        JophetType::UInt(bits) => {
                            if *i < 0 {
                                return Err(SemanticError::TypeError {
                                    message: format!(
                                        "Cannot assign negative literal {} to unsigned integer type UInt{}",
                                        i, bits
                                    ),
                                    span,
                                    file_path: self.current_module_path.clone(),
                                });
                            }
                            let max = if *bits == 64 {
                                u64::MAX as u128
                            } else {
                                (1u64 << bits) as u128
                            };
                            if (*i as u128) < max {
                                JophetType::UInt(*bits)
                            } else {
                                return Err(SemanticError::TypeError {
                                    message: format!(
                                        "Integer literal {} is out of range for type UInt{}",
                                        i, bits
                                    ),
                                    span,
                                    file_path: self.current_module_path.clone(),
                                });
                            }
                        }
                        JophetType::Float(32) => JophetType::Float(32),
                        JophetType::Float(64) => JophetType::Float(64),
                        _ => JophetType::Int(64),
                    }
                } else {
                    JophetType::Int(64)
                }
            }
            Literal::Float(_) => {
                if let Some(JophetType::Float(32)) = expected_type {
                    JophetType::Float(32)
                } else {
                    JophetType::Float(64)
                }
            }
            Literal::String(_) => {
                if let Some(JophetType::String) = expected_type {
                    JophetType::String
                } else {
                    JophetType::StringSlice
                }
            }
            Literal::Char(_) => JophetType::Char,
            Literal::Bool(_) => JophetType::Bool,
            Literal::Nothing => JophetType::Nothing,
        };

        if jophet_type == JophetType::String {
            let slice_expr = TypedExpression {
                kind: TypedExpressionKind::Literal(lit.clone()),
                jophet_type: JophetType::StringSlice,
                span: span.clone(),
            };
            return Ok(TypedExpression {
                kind: TypedExpressionKind::New {
                    jophet_type: JophetType::String,
                    args: vec![slice_expr],
                },
                jophet_type: JophetType::String,
                span,
            });
        }

        Ok(TypedExpression {
            kind: TypedExpressionKind::Literal(lit.clone()),
            jophet_type,
            span,
        })
    }

    /// Analyzes an interpolated string, analyzing each expression within it. It now
    /// validates that each expression is of a printable type.
    pub fn analyze_interpolated_string_expr(
        &mut self,
        parts: &[untyped::InterpolationPart],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let mut typed_parts = Vec::new();
        for part in parts {
            match part {
                untyped::InterpolationPart::Literal(s) => {
                    typed_parts.push(TypedInterpolationPart::Literal(s.clone()));
                }
                untyped::InterpolationPart::Expression(expr) => {
                    let typed_expr = self.analyze_expression(expr, ctx, None, errors);
                    if typed_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_expr); }
                    if !self.is_printable(&typed_expr.jophet_type) {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' cannot be formatted in an interpolated string.",
                                jophet_type_to_user_string(&typed_expr.jophet_type)
                            ),
                            span: expr.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    typed_parts.push(TypedInterpolationPart::Expression(typed_expr));
                }
            }
        }

        Ok(TypedExpression {
            kind: TypedExpressionKind::InterpolatedString(typed_parts),
            jophet_type: JophetType::String,
            span,
        })
    }
}