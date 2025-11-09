// src/core/semantic_analyzer/expressions/mod.rs
//! The main dispatcher for expression analysis in the semantic analyzer.
//!
//! This module contains the primary `analyze_expression` function, which matches on
//! the `untyped::ExpressionKind` and delegates to more specialized analysis functions
//! located in sibling modules. It now implements an error-collecting strategy: on failure,
//! an error is pushed to a vector and a sentinel `Error` expression is returned to
//! prevent error cascades.

use super::{ScopeContext, SemanticAnalyzer};
use crate::core::ast::typed::*;
use crate::core::ast::untyped;
use crate::diagnostics::errors::SemanticError;

pub mod access;
pub mod calls;
pub mod closures;
pub mod const_eval;
pub mod control_flow;
pub mod helpers;
pub mod instantiation;
pub mod literals;
pub mod monomorphization;
pub mod operators;

impl SemanticAnalyzer<'_> {
    /// The main entry point for analyzing an expression.
    ///
    /// This function acts as a dispatcher, matching on the `untyped::ExpressionKind`
    /// and calling the appropriate specialized analysis function. It takes an optional
    /// `expected_type` hint to support contextual type inference. If any sub-analysis
    /// fails, it pushes the error to the `errors` vector and returns a sentinel `Error`
    /// `TypedExpression` to prevent error cascades. For `const` calls, it produces a
    /// `ConstCall` node, which is evaluated in a separate pass by the functions that
    /// consume it (like `analyze_simple_variable_decl`).
    pub fn analyze_expression(
        &mut self,
        expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        expected_type: Option<&JophetType>,
        errors: &mut Vec<SemanticError>,
    ) -> TypedExpression {
        // This closure is a convenience to create a sentinel error expression.
        let make_error_expr = || TypedExpression {
            kind: TypedExpressionKind::Error,
            jophet_type: JophetType::ErrorSentinel,
            span: expr.span.clone(),
        };

        // All sub-analysis functions now return a Result. We match on it here.
        let result = match &expr.kind {
            untyped::ExpressionKind::ConstCall {
                name,
                generic_args,
                args,
            } => self.analyze_const_call_expr(name, generic_args, args, ctx, expr.span.clone(), errors),
            untyped::ExpressionKind::New {
                ty,
                generic_args,
                args,
            } => self.analyze_new_expr(ty, generic_args, args, ctx, expr.span.clone(), errors),
            untyped::ExpressionKind::Closure(decl) => {
                self.analyze_closure_expr(decl, ctx, expr.span.clone(), expected_type, errors)
            }
            untyped::ExpressionKind::Literal(lit) => {
                self.analyze_literal_expr(lit, expr.span.clone(), expected_type)
            }
            untyped::ExpressionKind::Identifier(name) => {
                self.analyze_identifier_expr(name, ctx, expr.span.clone())
            }
            untyped::ExpressionKind::BinaryOp(left, op, right) => {
                self.analyze_binary_op_expr(left, op, right, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::UnaryOp(op, right) => self.analyze_unary_op_expr(op, right, ctx, errors),
            untyped::ExpressionKind::FunctionCall {
                name,
                generic_args,
                args,
            } => self.analyze_function_call_expr(name, generic_args, args, ctx, expr.span.clone(), errors),
            untyped::ExpressionKind::InterpolatedString(parts) => {
                self.analyze_interpolated_string_expr(parts, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::StructInstantiation(name, generic_args, args) => self
                .analyze_struct_instantiation_expr(
                    name,
                    generic_args,
                    args,
                    ctx,
                    expr.span.clone(),
                    expected_type,
                    errors,
                ),
            untyped::ExpressionKind::TaggedUnionInstantiation {
                enum_name,
                variant_name,
                payload,
            } => self.analyze_tagged_union_instantiation_expr(
                enum_name,
                variant_name,
                payload,
                ctx,
                expr.span.clone(),
                errors,
            ),
            untyped::ExpressionKind::EnumVariantAccess {
                enum_name,
                variant_name,
            } => self.analyze_enum_variant_access_expr(enum_name, variant_name, ctx, expr.span.clone(), errors),
            untyped::ExpressionKind::MethodCall(object, name, args) => {
                self.analyze_method_call_expr(object, name, args, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::FieldAccess(object, field_name) => {
                self.analyze_field_access_expr(object, field_name, ctx, errors)
            }
            untyped::ExpressionKind::Tuple(elements) => {
                self.analyze_tuple_expr(elements, ctx, expr.span.clone(), expected_type, errors)
            }
            untyped::ExpressionKind::TupleAccess(object, index) => {
                self.analyze_tuple_access_expr(object, *index, ctx, errors)
            }
            untyped::ExpressionKind::ArrayLiteral(elements) => {
                self.analyze_array_literal_expr(elements, ctx, expr.span.clone(), expected_type, errors)
            }
            untyped::ExpressionKind::ArrayIndex { array, index } => {
                self.analyze_array_index_expr(array, index, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::ArraySlice { array, start, end } => {
                self.analyze_array_slice_expr(array, start, end, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::DictionaryInstantiation {
                key_type,
                value_type,
                pairs,
            } => {
                let resolved_key_type = match self.resolve_type(key_type, false, None, ctx, expr.span.clone()) {
                    Ok(t) => t,
                    Err(e) => {
                        errors.push(e);
                        // This is the key change: return the sentinel, not a Result
                        return make_error_expr();
                    }
                };
                let resolved_value_type = match self.resolve_type(value_type, false, None, ctx, expr.span.clone()) {
                    Ok(t) => t,
                    Err(e) => {
                        errors.push(e);
                        // Return the sentinel here as well
                        return make_error_expr();
                    }
                };
                // If both resolve calls succeed, we can proceed. The result of this call
                // will be handled by the outer match.
                self.analyze_dictionary_instantiation_expr(
                    &resolved_key_type,
                    &resolved_value_type,
                    &pairs
                        .iter()
                        .map(|(k, v)| untyped::Arg::KeyValuePair(k.clone(), v.clone()))
                        .collect::<Vec<_>>(),
                    ctx,
                    expr.span.clone(),
                    errors,
                )
            }
            untyped::ExpressionKind::Switch {
                expression,
                cases,
                else_block,
            } => self.analyze_switch_expression(
                expression,
                cases,
                else_block,
                ctx,
                expr.span.clone(),
                expected_type.is_some(),
                errors,
            ),
            untyped::ExpressionKind::Try(try_expr) => {
                self.analyze_try_expr(try_expr, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::Catch {
                expression,
                error_variable,
                body,
            } => self.analyze_catch_expr(expression, error_variable, body, ctx, expr.span.clone(), errors),
            untyped::ExpressionKind::AddressOf(addr_expr) => {
                self.analyze_address_of_expr(addr_expr, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::Dereference(deref_expr) => {
                self.analyze_dereference_expr(deref_expr, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::TernaryOp(cond, then, else_b) => self
                .analyze_ternary_op_expr(cond, then, else_b, ctx, expr.span.clone(), expected_type, errors),
            untyped::ExpressionKind::Allow(inner_expr) => {
                self.analyze_allow_expr(inner_expr, ctx, expr.span.clone(), errors)
            }
            untyped::ExpressionKind::Convert { expr, target_type } => {
                self.analyze_convert_expr(expr, target_type, ctx, expr.span.clone(), false, errors)
            }
            untyped::ExpressionKind::Parse {
                target_type,
                expr: parse_expr,
            } => self.analyze_parse_expr(target_type, parse_expr, ctx, expr.span.clone(), errors),
        };
        
        match result {
            Ok(typed_expr) => typed_expr,
            Err(e) => {
                errors.push(e);
                make_error_expr()
            }
        }
    }
}