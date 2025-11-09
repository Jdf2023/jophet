// src/core/semantic_analyzer/expressions/operators.rs
//! Semantic analysis for unary and binary operator expressions.
//!
//! This module handles type checking for all operator expressions, ensuring that
//! operands are compatible and determining the result type of the operation.
//! It has been updated to use the error-collecting paradigm.

use crate::core::ast::typed::*;
use crate::core::ast::{untyped, TokenKind};
use crate::core::semantic_analyzer::{
    context::BorrowState, types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer,
};
use crate::diagnostics::errors::SemanticError;

impl SemanticAnalyzer<'_> {
    /// Analyzes a binary operation, checking that the types of the left and right operands are compatible.
    pub fn analyze_binary_op_expr(
        &mut self,
        left: &untyped::Expression,
        op: &TokenKind,
        right: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        if *op == TokenKind::AsteriskAsterisk {
            let typed_left = self.analyze_expression(left, ctx, None, errors);
            if typed_left.jophet_type == JophetType::ErrorSentinel { return Ok(typed_left); }


            let expected_right_type = match typed_left.jophet_type {
                JophetType::Int(_) => Some(JophetType::UInt(64)), // Expect unsigned for integer base
                JophetType::UInt(_) => Some(JophetType::UInt(64)),
                JophetType::Float(_) => Some(typed_left.jophet_type.clone()), // Expect same float type
                _ => {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "The `**` operator is not supported for base type '{}'.",
                            jophet_type_to_user_string(&typed_left.jophet_type)
                        ),
                        span: left.span.clone(),
                        file_path: self.current_module_path.clone(),
                    })
                }
            };

            let typed_right = self.analyze_expression(right, ctx, expected_right_type.as_ref(), errors);
            if typed_right.jophet_type == JophetType::ErrorSentinel { return Ok(typed_right); }

            let final_type = typed_left.jophet_type.clone();

            match final_type {
                JophetType::Int(_) | JophetType::UInt(_) => {
                    if !matches!(typed_right.jophet_type, JophetType::UInt(_)) {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Exponent for an integer base must be an unsigned integer, but found type '{}'.\nHelp: To use a float exponent, convert the base to a float first, e.g., `convert({}, Float64) ** {}`",
                                jophet_type_to_user_string(&typed_right.jophet_type),
                                left.kind,
                                right.kind
                            ),
                            span: right.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                }
                JophetType::Float(_) => {
                    if typed_left.jophet_type != typed_right.jophet_type {
                        return Err(SemanticError::TypeError {
                            message: format!("Type mismatch for `**` operator. Base and exponent must be the same float type, but found '{}' and '{}'.", jophet_type_to_user_string(&typed_left.jophet_type), jophet_type_to_user_string(&typed_right.jophet_type)),
                            span: left.span.merge(&right.span),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                }
                _ => { /* Already handled above */ }
            }

            return Ok(TypedExpression {
                kind: TypedExpressionKind::BinaryOp(
                    Box::new(typed_left),
                    op.clone(),
                    Box::new(typed_right),
                ),
                jophet_type: final_type,
                span,
            });
        }

        let typed_left = self.analyze_expression(left, ctx, None, errors);
        if typed_left.jophet_type == JophetType::ErrorSentinel { return Ok(typed_left); }
        let typed_right = self.analyze_expression(right, ctx, Some(&typed_left.jophet_type), errors);
        if typed_right.jophet_type == JophetType::ErrorSentinel { return Ok(typed_right); }

        let jophet_type = if typed_left.jophet_type == typed_right.jophet_type {
            match op {
                TokenKind::EqualEqual
                | TokenKind::BangEquals
                | TokenKind::LAngle
                | TokenKind::RAngle
                | TokenKind::LessEquals
                | TokenKind::GreaterEquals
                | TokenKind::AmpersandAmpersand
                | TokenKind::PipePipe => JophetType::Bool,
                _ => typed_left.jophet_type.clone(),
            }
        } else {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Type mismatch in binary operation between {:?} and {:?}",
                    typed_left.jophet_type, typed_right.jophet_type
                ),
                span: left.span.merge(&right.span),
                file_path: self.current_module_path.clone(),
            });
        };
        Ok(TypedExpression {
            kind: TypedExpressionKind::BinaryOp(
                Box::new(typed_left),
                op.clone(),
                Box::new(typed_right),
            ),
            jophet_type,
            span,
        })
    }

    /// Analyzes a unary operation.
    pub fn analyze_unary_op_expr(
        &mut self,
        op: &TokenKind,
        right: &untyped::Expression,
        ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_right = self.analyze_expression(right, ctx, None, errors);
        if typed_right.jophet_type == JophetType::ErrorSentinel { return Ok(typed_right); }
        let span = right.span.clone();
        Ok(TypedExpression {
            jophet_type: typed_right.jophet_type.clone(),
            kind: TypedExpressionKind::UnaryOp(op.clone(), Box::new(typed_right)),
            span,
        })
    }

    /// Analyzes a ternary expression, ensuring the condition is a boolean and that both branches have the same type.
    pub fn analyze_ternary_op_expr(
        &mut self,
        cond: &untyped::Expression,
        then_b: &untyped::Expression,
        else_b: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        expected_type: Option<&JophetType>,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_cond = self.analyze_expression(cond, ctx, Some(&JophetType::Bool), errors);
        if typed_cond.jophet_type == JophetType::ErrorSentinel { return Ok(typed_cond); }
        if typed_cond.jophet_type != JophetType::Bool {
            return Err(SemanticError::TypeError {
                message: "Ternary operator condition must be a boolean expression.".to_string(),
                span: cond.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        let typed_then = self.analyze_expression(then_b, ctx, expected_type, errors);
        if typed_then.jophet_type == JophetType::ErrorSentinel { return Ok(typed_then); }
        let typed_else = self.analyze_expression(else_b, ctx, Some(&typed_then.jophet_type), errors);
        if typed_else.jophet_type == JophetType::ErrorSentinel { return Ok(typed_else); }

        if typed_then.jophet_type != typed_else.jophet_type {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Type mismatch between branches of ternary operator. Then branch has type {:?}, but else branch has type {:?}.",
                    typed_then.jophet_type, typed_else.jophet_type
                ),
                span: then_b.span.merge(&else_b.span),
                file_path: self.current_module_path.clone(),
            });
        }

        Ok(TypedExpression {
            kind: TypedExpressionKind::TernaryOp(
                Box::new(typed_cond),
                Box::new(typed_then.clone()),
                Box::new(typed_else),
            ),
            jophet_type: typed_then.jophet_type,
            span,
        })
    }

    /// Analyzes an address-of expression (`&`), creating a reference type.
    pub fn analyze_address_of_expr(
        &mut self,
        addr_expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        if let untyped::ExpressionKind::Identifier(name) = &addr_expr.kind {
            let info = ctx
                .symbol_table
                .get(name)
                .ok_or_else(|| SemanticError::NameError {
                    message: format!("Cannot find value `{}` in this scope", name),
                    span: addr_expr.span.clone(),
                    file_path: self.current_module_path.clone(),
                })?
                .clone();
            
            let typed_addr_expr = self.analyze_expression(addr_expr, ctx, None, errors);
            if typed_addr_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_addr_expr); }

            return Ok(TypedExpression {
                kind: TypedExpressionKind::AddressOf(Box::new(typed_addr_expr)),
                jophet_type: if info.is_mutable {
                    JophetType::MutableReference(Box::new(info.jophet_type.clone()))
                } else {
                    JophetType::Reference(Box::new(info.jophet_type.clone()))
                },
                span,
            });
        }
        Err(SemanticError::MemoryError {
            message: "Taking a reference of a temporary value is not allowed.".to_string(),
            span,
            file_path: self.current_module_path.clone(),
        })
    }

    /// Analyzes a dereference expression (`*`), checking for safety. Dereferencing a raw
    /// pointer is now only allowed inside an `allow` block.
    pub fn analyze_dereference_expr(
        &mut self,
        deref_expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_expr = self.analyze_expression(deref_expr, ctx, None, errors);
        if typed_expr.jophet_type == JophetType::ErrorSentinel { return Ok(typed_expr); }
        let inner_type = match &typed_expr.jophet_type {
            JophetType::Pointer(inner) => inner.as_ref().clone(),
            JophetType::Reference(inner) => inner.as_ref().clone(),
            JophetType::MutableReference(inner) => inner.as_ref().clone(),
            JophetType::RawPointer(inner) => {
                // This is the new safety check.
                if !ctx.in_allow_block {
                    return Err(SemanticError::MemoryError {
                        message: "Dereferencing a raw pointer is an unsafe operation and must be inside an `allow` block.".to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                inner.as_ref().clone()
            }
            _ => {
                return Err(SemanticError::TypeError {
                    message: "Cannot dereference a non-pointer/non-reference type.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                })
            }
        };
        Ok(TypedExpression {
            kind: TypedExpressionKind::Dereference(Box::new(typed_expr)),
            jophet_type: inner_type,
            span,
        })
    }

    /// Analyzes an identifier, looking it up in the symbol table and checking for
    /// moved, deleted, or mutably borrowed (frozen) values.
    pub fn analyze_identifier_expr(
        &self,
        name: &str,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<TypedExpression, SemanticError> {
        if ctx.deleted_vars.contains(name) {
            return Err(SemanticError::NameError {
                message: format!("Use of deleted value `{}`", name),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        if ctx.moved_vars.contains(name) {
            return Err(SemanticError::NameError {
                message: format!("Use of moved value `{}`", name),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        if let Some(BorrowState::MutablelyBorrowed) = ctx.borrow_states.get(name) {
            return Err(SemanticError::MemoryError {
                message: format!("Cannot use `{}` because it is currently borrowed as mutable", name),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        let info = ctx
            .symbol_table
            .get(name)
            .ok_or_else(|| SemanticError::NameError {
                message: format!("Undefined variable or module '{}'", name),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            })?;
        Ok(TypedExpression {
            kind: TypedExpressionKind::Identifier {
                name: name.to_string(),
                mangled_name: info.mangled_name.clone(),
            },
            jophet_type: info.jophet_type.clone(),
            span,
        })
    }
}