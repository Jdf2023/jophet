// src/backend/c/expressions/literals.rs
//! Handles the compilation of literals and simple operators.

use super::super::Generator;
use super::CExpression;
use crate::core::ast::typed::*;
use crate::core::ast::{Literal, TokenKind};

impl Generator {
    /// Compiles a literal value into its C string representation.
    pub(super) fn compile_literal_expression(
        &self,
        lit: &Literal,
        jophet_type: &JophetType,
    ) -> CExpression {
        let result = match lit {
            Literal::Int(i) => {
                if let JophetType::Int(64) = jophet_type {
                    format!("{}LL", i)
                } else {
                    i.to_string()
                }
            }
            Literal::Float(f) => f.to_string(),
            Literal::Char(c) => format!("'{}'", c.escape_default().to_string()),
            Literal::Bool(b) => b.to_string(),
            Literal::String(s) => format!("\"{}\"", s.escape_default().to_string()),
            Literal::Nothing => "(void)0".to_string(),
        };
        CExpression::Simple(result)
    }

    /// Compiles a unary operation expression (e.g., `-x`, `!y`) to its C equivalent.
    pub(super) fn compile_unary_op_expression(
        &mut self,
        op: &TokenKind,
        right: &TypedExpression,
    ) -> CExpression {
        let op_str = match op {
            TokenKind::Minus => "-",
            TokenKind::Bang => "!",
            TokenKind::Tilde => "~",
            _ => unreachable!("Semantic analysis should prevent invalid unary operators."),
        };
        let result = format!("({}{})", op_str, self.compile_expression(right));
        CExpression::Simple(result)
    }

    /// Compiles a ternary operation expression (`cond ? then : else`) to the C ternary operator.
    pub(super) fn compile_ternary_op_expression(
        &mut self,
        cond: &TypedExpression,
        then: &TypedExpression,
        else_b: &TypedExpression,
    ) -> CExpression {
        let result = format!(
            "({}) ? ({}) : ({})",
            self.compile_expression(cond),
            self.compile_expression(then),
            self.compile_expression(else_b)
        );
        CExpression::Simple(result)
    }

    /// Compiles a binary operation expression (e.g., `a + b`, `c == d`) to its C equivalent.
    pub(super) fn compile_binary_op_expression(
        &mut self,
        left: &TypedExpression,
        op: &TokenKind,
        right: &TypedExpression,
        span_start: usize,
    ) -> CExpression {
        let result = if *op == TokenKind::AsteriskAsterisk {
            let compiled_left = self.compile_expression(left);
            let compiled_right = self.compile_expression(right);
            match left.jophet_type {
                JophetType::Int(_) | JophetType::UInt(_) => {
                    self.runtime_needed = true;
                    // Casts are needed here because the runtime function expects int64_t
                    let base_c_type = self.jophet_type_to_c_string(&left.jophet_type);
                    let line = self.source_map.line_for_byte(span_start);

                    format!(
                        "({})jophet_int_pow((int64_t){}, (int64_t){}, \"{}\", {})",
                        base_c_type,
                        compiled_left,
                        compiled_right,
                        self.source_map.filename(),
                        line
                    )
                }
                JophetType::Float(_) => {
                    // Standard C `pow` for floats, which is fine.
                    format!("pow({}, {})", compiled_left, compiled_right)
                }
                _ => unreachable!("Semantic analysis should prevent `**` on non-numeric types"),
            }
        } else {
            let op_str = match op {
                TokenKind::Plus => "+",
                TokenKind::Minus => "-",
                TokenKind::Asterisk => "*",
                TokenKind::Slash => "/",
                TokenKind::Percent => "%",
                TokenKind::EqualEqual => "==",
                TokenKind::BangEquals => "!=",
                TokenKind::LAngle => "<",
                TokenKind::RAngle => ">",
                TokenKind::LessEquals => "<=",
                TokenKind::GreaterEquals => ">=",
                TokenKind::AmpersandAmpersand => "&&",
                TokenKind::PipePipe => "||",
                TokenKind::Ampersand => "&",
                TokenKind::Pipe => "|",
                TokenKind::Caret => "^",
                TokenKind::LessLess => "<<",
                TokenKind::GreaterGreater => ">>",
                _ => panic!("Unsupported binary operator in backend: {:?}", op),
            };
            format!(
                "({}) {} ({})",
                self.compile_expression(left),
                op_str,
                self.compile_expression(right)
            )
        };
        CExpression::Simple(result)
    }
}