// src/core/parser/statements.rs
//! Contains the parsing logic for statements in the Jophet language.
//!
//! This module implements parsing for imperative constructs like `if`, `while`, `for`,
//! assignments, and `delete`. It now supports a conditional `if let`-style binding
//! for unwrapping fallible types.

use super::{Parser};
use crate::core::ast::untyped::*;
use crate::core::ast::TokenKind;
use crate::diagnostics::errors::ParserError;

impl Parser {
    /// Parses a single statement, now with context about whether `yield` is allowed.
    /// This is the main dispatch function for the statement-level grammar.
    /// It now correctly distinguishes between declarations and assignments.
    ///
    /// # Arguments
    /// * `allow_yield` - If `true`, the `yield` keyword is parsed as a valid statement.
    pub(super) fn parse_statement(&mut self, allow_yield: bool, errors: &mut Vec<ParserError>) -> Option<Statement> {
        let doc_comment = self.parse_optional_doc_comment(errors).flatten();

        // Check for an optional `public` visibility modifier.
        let is_public = if *self.current_kind() == TokenKind::Public {
            self.advance();
            true
        } else {
            false
        };

        // Optional `const` qualifier for declarations.
        let is_const_decl = if *self.current_kind() == TokenKind::Const {
            self.advance();
            true
        } else {
            false
        };

        let start_span = self.current_span();

        // Dispatch to the specific statement parsing function based on the current token.
        let kind = match self.current_kind() {
            TokenKind::Delete => self.parse_delete_statement(errors),
            TokenKind::Import => self.parse_import_statement(errors),
            TokenKind::ModuleDocComment(_) => {
                errors.push(ParserError::UnexpectedToken {
                    expected: "a statement or definition".to_string(),
                    found: "a module-level doc comment (`//!`)".to_string(),
                    span: self.current_span(),
                });
                return None;
            }
            TokenKind::Function => {
                // Look ahead to see if a name follows. If not, it's a closure expression.
                if self.peek_at(1).kind.is_identifier() {
                    self.parse_function_like(doc_comment, false, is_public, is_const_decl, errors)
                        .map(StatementKind::FunctionDecl)
                } else {
                    self.parse_assignment_or_expression_statement(errors)
                }
            }
            TokenKind::Struct => self.parse_struct_def(doc_comment, is_public, errors),
            TokenKind::Enum => self.parse_enum_def(doc_comment, is_public, errors),
            TokenKind::Union => self.parse_union_def(doc_comment, is_public, errors),
            TokenKind::TaggedUnion => self.parse_tagged_union_def(doc_comment, is_public, errors),
            TokenKind::Error => self.parse_error_def(doc_comment, is_public, errors),
            TokenKind::Trait => self.parse_trait_def(doc_comment, is_public, errors),
            TokenKind::Implement => self.parse_implement_block(doc_comment, errors),
            TokenKind::If => self.parse_if_statement(errors).map(StatementKind::If),
            TokenKind::While => self.parse_while_statement(errors).map(StatementKind::While),
            TokenKind::For => self.parse_for_statement(errors).map(StatementKind::For),
            TokenKind::Switch => self
                .parse_expression(errors)
                .map(StatementKind::ExpressionStatement),
            TokenKind::Break => {
                self.advance();
                Some(StatementKind::Break)
            }
            TokenKind::Continue => {
                self.advance();
                Some(StatementKind::Continue)
            }
            TokenKind::Return => {
                self.advance();
                let expr = self.parse_expression(errors)?;
                Some(StatementKind::Return(expr))
            }
            TokenKind::Yield if allow_yield => {
                self.advance();
                let expr = self.parse_expression(errors)?;
                Some(StatementKind::Yield(expr))
            }
            TokenKind::Mutable => {
                self.advance();
                self.parse_variable_declaration(true, is_const_decl, errors)
            }
            _ => {
                // This is the key logic to distinguish declarations from assignments/expressions.
                if self.is_declaration() {
                    self.parse_variable_declaration(false, is_const_decl, errors)
                } else {
                    self.parse_assignment_or_expression_statement(errors)
                }
            }
        }?;
        let end_span = self.tokens.get(self.pos - 1).map_or(start_span.clone(), |t| t.span.clone());
        Some(Statement {
            kind,
            span: start_span.merge(&end_span),
        })
    }

    /// Parses an immediate `delete` statement.
    /// Example: `delete my_string`
    pub fn parse_delete_statement(&mut self, errors: &mut Vec<ParserError>) -> Option<StatementKind> {
        self.eat(&TokenKind::Delete, errors)?;
        let (name, _) = self.expect_identifier(errors)?;
        Some(StatementKind::Delete(name))
    }

    /// Parses an `if-else if-else` statement chain.
    /// It now handles two forms for the condition:
    /// 1. A standard boolean expression: `if x > 10 ...`
    /// 2. A conditional binding (if-let): `if y: Type = fallible_expr? ...`
    pub fn parse_if_statement(&mut self, errors: &mut Vec<ParserError>) -> Option<IfStatement> {
        self.eat(&TokenKind::If, errors)?;
        
        // --- NEW, MORE ROBUST LOOKAHEAD LOGIC ---
        // Look ahead to see if this is an `if let`-style binding.
        // The pattern is `identifier : Type =`
        let is_conditional_binding = {
            // Create a temporary clone of the parser to perform a "trial parse"
            // without affecting the main parser's state.
            let mut temp_parser = self.clone();
            
            // Try to parse the binding pattern: `identifier : Type`
            let is_pattern = if temp_parser.current_kind().is_identifier() {
                temp_parser.advance(); // consume identifier
                if *temp_parser.current_kind() == TokenKind::Colon {
                    temp_parser.advance(); // consume colon
                    // Try to parse a full type annotation.
                    let mut dummy_errors = Vec::new(); // Dummy for trial parse
                    temp_parser.parse_type(&mut dummy_errors).is_some()
                } else {
                    false
                }
            } else {
                false
            };

            // If the pattern parsed successfully, check if it's followed by `=`.
            if is_pattern && *temp_parser.current_kind() == TokenKind::Equal {
                true
            } else {
                false
            }
        };
        
        let condition;
        let binding = if is_conditional_binding {
            let (var_name, _) = self.expect_identifier(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            let var_type = self.parse_type(errors)?;
            self.eat(&TokenKind::Equal, errors)?;
            let initializer = self.parse_expression(errors)?;
            condition = initializer; // The expression being evaluated is the condition
            Some((var_name, var_type))
        } else {
            condition = self.parse_expression(errors)?;
            None
        };
        
        self.skip_newlines();

        // An `if` block is terminated by either `else` or `end`. Yield is not allowed.
        let then_block = self.parse_block(&[TokenKind::Else, TokenKind::End], false, errors);

        let else_block = if *self.current_kind() == TokenKind::Else {
            self.eat(&TokenKind::Else, errors)?;
            self.skip_newlines();

            // If `else` is followed by `if`, it's an `else if` chain.
            if *self.current_kind() == TokenKind::If {
                Some(Box::new(ElseBlock::ElseIf(self.parse_if_statement(errors)?)))
            } else {
                // Otherwise, it's a final `else` block, terminated by `end`.
                let final_else_block = self.parse_block(&[TokenKind::End], false, errors);
                self.eat(&TokenKind::End, errors)?;
                Some(Box::new(ElseBlock::Else(final_else_block)))
            }
        } else {
            // If there's no `else`, the block must be closed by `end`.
            self.eat(&TokenKind::End, errors)?;
            None
        };

        Some(IfStatement {
            condition,
            binding,
            then_block,
            else_block,
        })
    }

    /// Parses a `while` loop.
    pub fn parse_while_statement(&mut self, errors: &mut Vec<ParserError>) -> Option<WhileStatement> {
        self.eat(&TokenKind::While, errors)?;
        let condition = self.parse_expression(errors)?;
        self.skip_newlines();
        // A `while` loop body is terminated by `end`. Yield is not allowed.
        let body = self.parse_block(&[TokenKind::End], false, errors);
        self.eat(&TokenKind::End, errors)?;
        Some(WhileStatement { condition, body })
    }

    /// Parses a `for` loop.
    /// It now supports two forms:
    /// 1. `for i = start:stop` (step of 1) or `for i = start:step:stop` (custom step)
    /// 2. `for item in collection`
    pub fn parse_for_statement(&mut self, errors: &mut Vec<ParserError>) -> Option<ForStatement> {
        self.eat(&TokenKind::For, errors)?;
        let (iterator_name, _) = self.expect_identifier(errors)?;

        let kind = if *self.current_kind() == TokenKind::Equal {
            // --- Path for Numeric Range Loop ---
            self.eat(&TokenKind::Equal, errors)?;
            let first_expr = self.parse_expression(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            let second_expr = self.parse_expression(errors)?;
            let (start, step, stop) = if *self.current_kind() == TokenKind::Colon {
                self.eat(&TokenKind::Colon, errors)?;
                let third_expr = self.parse_expression(errors)?;
                (first_expr, Some(second_expr), third_expr)
            } else {
                (first_expr, None, second_expr)
            };
            Some(ForLoopKind::Range { start, stop, step })
        } else if *self.current_kind() == TokenKind::In {
            // --- Path for Iterable Loop ---
            self.eat(&TokenKind::In, errors)?;
            let collection = self.parse_expression(errors)?;
            Some(ForLoopKind::Iterable { collection })
        } else {
            errors.push(ParserError::UnexpectedToken {
                expected: "'=' or 'in' after for loop variable".to_string(),
                found: format!("{:?}", self.current_kind()),
                span: self.current_span(),
            });
            None
        }?;

        self.skip_newlines();
        let body = self.parse_block(&[TokenKind::End], false, errors);
        self.eat(&TokenKind::End, errors)?;
        Some(ForStatement {
            iterator_name,
            kind,
            body,
        })
    }

    /// Parses either an assignment statement or an expression statement.
    ///
    /// This function now handles simple assignments, compound assignments, tuple destructuring
    /// assignments, and array destructuring assignments. It first parses a potential
    /// left-hand side and then looks at the following token to decide what kind of statement it is.
    pub fn parse_assignment_or_expression_statement(
        &mut self,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        // First, attempt to parse the left-hand side. This could be a simple
        // expression (`x`, `s.field`), a tuple of identifiers `(x, y)`, or an array `[x, y]`.
        let lvalue = if *self.current_kind() == TokenKind::LParen {
            // Potentially a tuple destructuring assignment.
            self.eat(&TokenKind::LParen, errors)?; // consume `(`
            let mut names = Vec::new();
            while *self.current_kind() != TokenKind::RParen {
                let (name, _) = self.expect_identifier(errors)?;
                names.push(name);
                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&TokenKind::RParen, errors)?;

            // If we have a tuple of names, the next token MUST be an equals sign.
            // A tuple of identifiers like `(x, y)` is not a valid standalone expression.
            if *self.current_kind() != TokenKind::Equal {
                errors.push(ParserError::UnexpectedToken {
                    expected: "an '=' for destructuring assignment".to_string(),
                    found: format!("{:?}", self.current_kind()),
                    span: self.current_span(),
                });
                return None;
            }

            AssignmentLValue::Tuple(names)
        } else if *self.current_kind() == TokenKind::LBracket {
            // Potentially an array destructuring assignment.
            self.eat(&TokenKind::LBracket, errors)?; // consume `[`
            let mut names = Vec::new();
            while *self.current_kind() != TokenKind::RBracket {
                let (name, _) = self.expect_identifier(errors)?;
                names.push(name);
                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
            self.eat(&TokenKind::RBracket, errors)?;

            // Similar to tuples, an array pattern must be followed by an assignment.
            if *self.current_kind() != TokenKind::Equal {
                errors.push(ParserError::UnexpectedToken {
                    expected: "an '=' for destructuring assignment".to_string(),
                    found: format!("{:?}", self.current_kind()),
                    span: self.current_span(),
                });
                return None;
            }

            AssignmentLValue::Array(names)
        } else {
            // Otherwise, it's a standard expression.
            AssignmentLValue::Expression(self.parse_expression(errors)?)
        };

        // Now, look at the token after the l-value to determine the statement type.
        let op_token = self.current().clone();
        match op_token.kind {
            // Simple or destructuring assignment: `lvalue = rvalue`
            TokenKind::Equal => {
                self.advance();
                let right = self.parse_expression(errors)?;
                Some(StatementKind::Assignment(lvalue, right))
            }
            // Compound assignment: `x += 1`. This is only valid for simple expression l-values.
            op @ (TokenKind::PlusEquals
            | TokenKind::MinusEquals
            | TokenKind::AsteriskEquals
            | TokenKind::AsteriskAsteriskEquals
            | TokenKind::SlashEquals
            | TokenKind::PercentEquals
            | TokenKind::AmpersandEquals
            | TokenKind::PipeEquals
            | TokenKind::CaretEquals
            | TokenKind::LessLessEquals
            | TokenKind::GreaterGreaterEquals) => {
                // Ensure the l-value was a simple expression, not a tuple/array pattern.
                if let AssignmentLValue::Expression(left_expr) = lvalue {
                    self.advance();
                    let right = self.parse_expression(errors)?;
                    // Desugar `x op= y` into `x = x op y`.
                    let binary_op_token = match op {
                        TokenKind::PlusEquals => TokenKind::Plus,
                        TokenKind::MinusEquals => TokenKind::Minus,
                        TokenKind::AsteriskEquals => TokenKind::Asterisk,
                        TokenKind::AsteriskAsteriskEquals => TokenKind::AsteriskAsterisk,
                        TokenKind::SlashEquals => TokenKind::Slash,
                        TokenKind::PercentEquals => TokenKind::Percent,
                        TokenKind::AmpersandEquals => TokenKind::Ampersand,
                        TokenKind::PipeEquals => TokenKind::Pipe,
                        TokenKind::CaretEquals => TokenKind::Caret,
                        TokenKind::LessLessEquals => TokenKind::LessLess,
                        TokenKind::GreaterGreaterEquals => TokenKind::GreaterGreater,
                        _ => unreachable!(),
                    };
                    let desugared_span = left_expr.span.merge(&right.span);
                    let desugared = Expression {
                        kind: ExpressionKind::BinaryOp(
                            Box::new(left_expr.clone()),
                            binary_op_token,
                            Box::new(right),
                        ),
                        span: desugared_span,
                    };
                    Some(StatementKind::Assignment(
                        AssignmentLValue::Expression(left_expr),
                        desugared,
                    ))
                } else {
                    errors.push(ParserError::UnexpectedToken {
                        expected: "a simple variable or field".to_string(),
                        found: "a tuple or array pattern before a compound assignment operator".to_string(),
                        span: op_token.span,
                    });
                    None
                }
            }
            // If no assignment operator follows, it must be an expression statement.
            // This also implies the l-value must have been a simple expression.
            _ => {
                if let AssignmentLValue::Expression(expr) = lvalue {
                    Some(StatementKind::ExpressionStatement(expr))
                } else {
                    // This case should have been caught earlier (a pattern not followed by '=').
                    // This is a safeguard.
                    errors.push(ParserError::UnexpectedToken {
                        expected: "an assignment".to_string(),
                        found: "a tuple or array pattern without an assignment".to_string(),
                        span: self.current_span(),
                    });
                    None
                }
            }
        }
    }
}