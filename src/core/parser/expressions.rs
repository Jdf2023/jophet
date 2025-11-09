// src/core/parser/expressions.rs
//! Contains the parsing logic for expressions in the Jophet language.
//!
//! This module implements a Pratt-style parser for expressions, correctly handling
//! operator precedence and associativity. The entry point is `parse_expression`,
//! which delegates to a chain of functions, each responsible for a specific
//! level of operator precedence (e.g., `parse_term` for `+`/`-`, `parse_factor`
//! for `*`/`/`). All parsing functions have been updated to collect errors and
//! return `Option<T>` on failure.

use super::Parser;
use crate::core::ast::untyped::*;
use crate::core::ast::{Literal, Span, TokenKind};
use crate::diagnostics::errors::ParserError;

impl Parser {
    /// The main entry point for parsing any expression.
    /// It starts the precedence climbing by calling the parser for the lowest
    /// precedence operator, the ternary `? :`.
    pub fn parse_expression(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_ternary(errors)
    }

    /// Parses a ternary expression (`condition ? then : else`).
    /// This is the lowest precedence operator.
    fn parse_ternary(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        let mut condition = self.parse_logical_or(errors)?;
        if *self.current_kind() == TokenKind::Question {
            self.advance();
            let then_branch = self.parse_expression(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            let else_branch = self.parse_ternary(errors)?;
            let span = condition.span.merge(&else_branch.span);
            condition = Expression {
                kind: ExpressionKind::TernaryOp(
                    Box::new(condition),
                    Box::new(then_branch),
                    Box::new(else_branch),
                ),
                span,
            };
        }
        Some(condition)
    }

    /// A generic helper function for parsing left-associative binary operators
    /// at a given precedence level.
    ///
    /// # Arguments
    /// * `next_parser` - A function pointer to the parser for the next higher
    ///   precedence level.
    /// * `tokens` - A slice of `TokenKind`s that this function should match.
    fn parse_binary_op(
        &mut self,
        next_parser: fn(&mut Self, &mut Vec<ParserError>) -> Option<Expression>,
        tokens: &[TokenKind],
        errors: &mut Vec<ParserError>,
    ) -> Option<Expression> {
        let mut left = next_parser(self, errors)?;
        while tokens
            .iter()
            .any(|t| std::mem::discriminant(t) == std::mem::discriminant(self.current_kind()))
        {
            let op = self.current_kind().clone();
            self.advance();
            let right = next_parser(self, errors)?;
            let span = left.span.merge(&right.span);
            left = Expression {
                kind: ExpressionKind::BinaryOp(Box::new(left), op, Box::new(right)),
                span,
            };
        }
        Some(left)
    }

    /// Parses logical OR expressions (`||`).
    fn parse_logical_or(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(Self::parse_logical_and, &[TokenKind::PipePipe], errors)
    }

    /// Parses logical AND expressions (`&&`).
    fn parse_logical_and(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(Self::parse_bitwise_or, &[TokenKind::AmpersandAmpersand], errors)
    }

    /// Parses bitwise OR expressions (`|`).
    fn parse_bitwise_or(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(Self::parse_bitwise_xor, &[TokenKind::Pipe], errors)
    }

    /// Parses bitwise XOR expressions (`^`).
    fn parse_bitwise_xor(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(Self::parse_bitwise_and, &[TokenKind::Caret], errors)
    }

    /// Parses bitwise AND expressions (`&`).
    fn parse_bitwise_and(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(Self::parse_equality, &[TokenKind::Ampersand], errors)
    }

    /// Parses equality expressions (`==`, `!=`).
    fn parse_equality(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(
            Self::parse_comparison,
            &[TokenKind::EqualEqual, TokenKind::BangEquals],
            errors,
        )
    }

    /// Parses comparison expressions (`<`, `>`, `<=`, `>=`).
    fn parse_comparison(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(
            Self::parse_shift,
            &[
                TokenKind::LAngle,
                TokenKind::RAngle,
                TokenKind::LessEquals,
                TokenKind::GreaterEquals,
            ],
            errors,
        )
    }

    /// Parses bitwise shift expressions (`<<`, `>>`).
    fn parse_shift(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(
            Self::parse_term,
            &[TokenKind::LessLess, TokenKind::GreaterGreater],
            errors,
        )
    }

    /// Parses additive expressions (`+`, `-`).
    fn parse_term(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(Self::parse_factor, &[TokenKind::Plus, TokenKind::Minus], errors)
    }

    /// Parses multiplicative expressions (`*`, `/`, `%`).
    fn parse_factor(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.parse_binary_op(
            Self::parse_power,
            &[
                TokenKind::Asterisk,
                TokenKind::Slash,
                TokenKind::Percent,
            ],
            errors,
        )
    }

    /// Parses exponentiation expressions (`**`), which are right-associative.
    fn parse_power(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        let mut left = self.parse_unary(errors)?;
        if *self.current_kind() == TokenKind::AsteriskAsterisk {
            let op = self.current_kind().clone();
            self.advance();
            // Recurse on `parse_power` for right-associativity
            let right = self.parse_power(errors)?;
            let span = left.span.merge(&right.span);
            left = Expression {
                kind: ExpressionKind::BinaryOp(Box::new(left), op, Box::new(right)),
                span,
            };
        }
        Some(left)
    }

    /// Parses unary prefix expressions (`-`, `!`, `~`, `allow`).
    fn parse_unary(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        if matches!(
            self.current_kind(),
            TokenKind::Bang | TokenKind::Minus | TokenKind::Tilde | TokenKind::Allow
        ) {
            let op_token = self.advance().clone();
            let op = op_token.kind;
            let right = self.parse_unary(errors)?;
            let span = op_token.span.merge(&right.span);
            let kind = if op == TokenKind::Allow {
                ExpressionKind::Allow(Box::new(right))
            } else {
                ExpressionKind::UnaryOp(op, Box::new(right))
            };
            Some(Expression { kind, span })
        } else {
            self.parse_postfix(errors)
        }
    }

    /// Parses postfix expressions like function calls, method calls, field access,
    /// array indexing, and `catch`. It uses lookahead to disambiguate generic function
    /// calls from `<` comparisons. It also handles array/vector indexing (`[]`) and
    /// open-ended slicing (`[start:end]`, `[start:]`, `[:end]`, `[:]`), correctly
    /// interpreting the `end` keyword as an identifier within this context.
    fn parse_postfix(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        let mut expr = self.parse_primary(errors)?;
        loop {
            let is_generic_call = if self.current_kind() == &TokenKind::LAngle {
                // Potential generic call. We need to look ahead to disambiguate from a '<' comparison.
                if let ExpressionKind::Identifier(_) = &expr.kind {
                    // Find the matching '>'
                    if let Some(rangle_pos) = self.find_matching_rangle(self.pos) {
                        // If the token immediately after the '>' is '(', it's a generic call.
                        if let Some(token_after) = self.tokens.get(rangle_pos + 1) {
                            token_after.kind == TokenKind::LParen
                        } else {
                            false // Reached end of file
                        }
                    } else {
                        false // No matching '>' found
                    }
                } else {
                    false // It's not `Identifier < ...`, so it must be a comparison.
                }
            } else {
                false
            };

            if is_generic_call {
                // We've confirmed it's a generic call, so parse it.
                let name = if let ExpressionKind::Identifier(n) = expr.kind { n } else { unreachable!() };
                let generic_args = self.parse_generic_argument_list(errors)?; // Consumes `<...>`
                let (args, end_span) = self.parse_function_call_args(errors)?; // Consumes `(...)`

                let span = expr.span.merge(&end_span);
                expr = Expression {
                    kind: ExpressionKind::FunctionCall { name, generic_args, args },
                    span,
                };
            } else if *self.current_kind() == TokenKind::LParen {
                // Standard (non-generic) function/method call.
                let start_span = expr.span.clone();
                let (args, end_span) = self.parse_function_call_args(errors)?;
                let span = start_span.merge(&end_span);
                expr = match expr.kind {
                    ExpressionKind::Identifier(name) => Expression {
                        kind: ExpressionKind::FunctionCall { name, generic_args: Vec::new(), args },
                        span,
                    },
                    ExpressionKind::FieldAccess(object, field) => Expression {
                        kind: ExpressionKind::MethodCall(object, field, args),
                        span,
                    },
                    _ => {
                        errors.push(ParserError::InvalidCallTarget { span });
                        return None;
                    }
                };
            } else if *self.current_kind() == TokenKind::Dot {
                self.advance();
                let end_span;
                let kind = match self.current_kind().clone() {
                    TokenKind::Identifier(field_name) => {
                        self.advance(); // consume method/field name
                        end_span = self.current_span();
                        ExpressionKind::FieldAccess(Box::new(expr.clone()), field_name)
                    }
                    TokenKind::Type(field_name) => {
                        let field_span = self.current_span();
                        self.advance();
                        end_span = field_span;
                        let enum_name = if let ExpressionKind::Identifier(n) = expr.kind {
                            n
                        } else {
                            errors.push(ParserError::InvalidTypeExpression {
                                span: expr.span.clone(),
                            });
                            return None;
                        };
                        ExpressionKind::EnumVariantAccess {
                            enum_name,
                            variant_name: field_name,
                        }
                    }
                    TokenKind::IntLiteral(index) => {
                        let index_span = self.current_span();
                        self.advance();
                        end_span = index_span;
                        ExpressionKind::TupleAccess(Box::new(expr.clone()), index as usize)
                    }
                    _ => {
                        errors.push(ParserError::ExpectedFieldOrTupleIndex { span: self.current_span() });
                        return None;
                    }
                };
                expr = Expression {
                    kind,
                    span: expr.span.merge(&end_span),
                };
            } else if *self.current_kind() == TokenKind::LBracket {
                self.advance(); // consume '['

                // Check for an optional start expression.
                let start_expr = if *self.current_kind() == TokenKind::Colon {
                    // This is a slice starting from the beginning, e.g., `[:end]` or `[:]`
                    None
                } else if *self.current_kind() == TokenKind::End {
                    // This is the fix: explicitly handle the `end` keyword as an identifier here.
                    let token = self.advance().clone();
                    Some(Expression {
                        kind: ExpressionKind::Identifier("end".to_string()),
                        span: token.span,
                    })
                } else {
                    // It's a regular expression, parse it normally.
                    Some(self.parse_expression(errors)?)
                };

                // Check if it's a slice or a simple index.
                if *self.current_kind() == TokenKind::Colon {
                    // It's a slice
                    self.advance(); // consume ':'

                    // Check for an optional end expression.
                    let end_expr = if *self.current_kind() == TokenKind::RBracket {
                        // This is a slice going to the end, e.g., `[start:]` or `[:]`
                        None
                    } else if *self.current_kind() == TokenKind::End {
                        // This is the fix: explicitly handle the `end` keyword as an identifier here.
                        let token = self.advance().clone();
                        Some(Expression {
                            kind: ExpressionKind::Identifier("end".to_string()),
                            span: token.span,
                        })
                    } else {
                        // It's a regular expression.
                        Some(self.parse_expression(errors)?)
                    };
                    
                    let rbracket = self.eat(&TokenKind::RBracket, errors)?;
                    let span = expr.span.merge(&rbracket.span);
                    expr = Expression {
                        kind: ExpressionKind::ArraySlice {
                            array: Box::new(expr),
                            start: start_expr.map(Box::new),
                            end: end_expr.map(Box::new),
                        },
                        span,
                    };
                } else {
                    // It's a simple index.
                    // The start_expr we parsed must be Some(...), otherwise it's a syntax error.
                    let index_expr = start_expr.ok_or_else(|| {
                        errors.push(ParserError::UnexpectedToken {
                            expected: "an index expression or slice".to_string(),
                            found: format!("{:?}", self.current_kind()),
                            span: self.current_span(),
                        });
                        // Return a dummy error; the Option will be handled by the caller.
                        ParserError::SyntaxError { message: "dummy".to_string(), span: self.current_span() }
                    }).ok()?; // Propagate None on error

                    let rbracket = self.eat(&TokenKind::RBracket, errors)?;
                    let span = expr.span.merge(&rbracket.span);
                    expr = Expression {
                        kind: ExpressionKind::ArrayIndex { 
                            array: Box::new(expr), 
                            index: Box::new(index_expr),
                        },
                        span,
                    };
                }
            } else if *self.current_kind() == TokenKind::Catch {
                self.advance(); // consume 'catch'
                let (error_variable, _) = self.expect_identifier(errors)?;
                self.eat(&TokenKind::Do, errors)?;
                self.skip_newlines();
                // A `catch` block body is terminated by `end`. `yield` is now allowed.
                let body = self.parse_block(&[TokenKind::End], true, errors);
                let end_token = self.eat(&TokenKind::End, errors)?;
                let span = expr.span.merge(&end_token.span);
                expr = Expression {
                    kind: ExpressionKind::Catch {
                        expression: Box::new(expr),
                        error_variable,
                        body,
                    },
                    span,
                };
            } else {
                // No more postfix operations found. Break the loop.
                break;
            }
        }
        Some(expr)
    }


    /// A public helper method used by the semantic analyzer to parse the content
    /// of a string literal that is intended for interpolation. It finds `{...}` blocks,
    /// re-tokenizes and re-parses their content as expressions, and separates the
    /// literal parts of the string.
    pub fn parse_interpolated_string_content(
        &self,
        content: &str,
        span: Span,
    ) -> Result<Vec<InterpolationPart>, ParserError> {
        let mut parts = Vec::new();
        let mut last_end = 0;
        let mut chars = content.char_indices().peekable();

        while let Some((start, c)) = chars.next() {
            if c == '{' {
                // Add the preceding literal part.
                if start > last_end {
                    parts.push(InterpolationPart::Literal(
                        content[last_end..start].to_string(),
                    ));
                }

                // Find the matching closing brace `}`.
                let mut balance = 1;
                let mut end_expr_offset = None;
                
                // Clone the iterator to find the end without consuming the main one
                let mut inner_chars = chars.clone();
                while let Some((i, c_inner)) = inner_chars.next() {
                     if c_inner == '{' {
                        balance += 1;
                    }
                    if c_inner == '}' {
                        balance -= 1;
                    }
                    if balance == 0 {
                        end_expr_offset = Some(i);
                        break;
                    }
                }

                let end_expr = end_expr_offset.ok_or_else(|| ParserError::UnmatchedBraceInFormatString {
                    span: span.clone(),
                })?;

                // Isolate, tokenize, and parse the expression string.
                let expr_str = &content[start + 1..end_expr];
                let tokens = crate::core::lexer::tokenize(expr_str, self.current_file.clone()).map_err(|_| {
                    ParserError::InvalidExpressionInFormatString { span: span.clone() }
                })?;
                let mut expr_parser =
                    crate::core::parser::Parser::new(tokens, self.current_file.clone());
                
                let mut temp_errors = Vec::new();
                let parsed_expr = expr_parser.parse_expression(&mut temp_errors).ok_or_else(|| temp_errors.pop().unwrap_or(
                     ParserError::InvalidExpressionInFormatString { span: span.clone() }
                ))?;

                parts.push(InterpolationPart::Expression(parsed_expr));
                
                // Advance main iterator past the parsed expression
                last_end = end_expr + 1;
                while chars.peek().map_or(false, |(i, _)| *i < last_end) {
                    chars.next();
                }
            }
        }

        // Add any remaining literal part after the last expression.
        if last_end < content.len() {
            parts.push(InterpolationPart::Literal(
                content[last_end..].to_string(),
            ));
        }

        Ok(parts)
    }

    /// Parses primary expressions, which are the atoms of the expression grammar.
    /// This includes literals, identifiers, parenthesized expressions, `new` expressions,
    /// and `const` function calls. It now also parses anonymous `function` expressions
    /// for creating closures.
    fn parse_primary(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        let token = self.current().clone();
        let kind = match token.kind {
            TokenKind::Const => {
                self.advance(); // consume 'const'
                // A `const` keyword must be followed by a function call expression.
                // We parse a full expression here to correctly handle chained calls like `const foo().bar()`
                let call_expr = self.parse_expression(errors)?;
                if let ExpressionKind::FunctionCall {
                    name,
                    generic_args,
                    args,
                } = call_expr.kind
                {
                    Some(ExpressionKind::ConstCall {
                        name,
                        generic_args,
                        args,
                    })
                } else {
                    errors.push(ParserError::SyntaxError {
                        message: "The `const` keyword must be followed by a function call."
                            .to_string(),
                        span: token.span.merge(&call_expr.span),
                    });
                    return None;
                }
            }
            TokenKind::New => {
                self.advance();
                let ty = self.parse_type(errors)?;
                let generic_args = if self.current_kind() == &TokenKind::LAngle {
                    self.parse_generic_argument_list(errors)?
                } else {
                    Vec::new()
                };

                // Check if this is a dictionary to use the special key-value parser.
                let is_dictionary = match &ty {
                    Type::Generic(name, _) if name == "Dictionary" => true,
                    Type::Simple(name) if name == "Dictionary" => true,
                    _ => false
                };

                let (args, _) = if is_dictionary {
                    self.parse_dictionary_argument_list(errors)?
                } else {
                    self.parse_argument_list(errors)?
                };

                Some(ExpressionKind::New { ty, generic_args, args })
            }
            TokenKind::Function => {
                // This is an anonymous function expression (a closure).
                // We parse it like a function declaration but without a name.
                let closure_decl = self.parse_function_like(None, false, false, false, errors)?;
                Some(ExpressionKind::Closure(closure_decl))
            }
            TokenKind::IntLiteral(i) => {
                self.advance();
                Some(ExpressionKind::Literal(Literal::Int(i)))
            }
            TokenKind::FloatLiteral(f) => {
                self.advance();
                Some(ExpressionKind::Literal(Literal::Float(f)))
            }
            TokenKind::StringLiteral(s) => {
                self.advance();
                Some(ExpressionKind::Literal(Literal::String(s)))
            }
            TokenKind::CharLiteral(c) => {
                self.advance();
                Some(ExpressionKind::Literal(Literal::Char(c)))
            }
            TokenKind::BoolLiteral(b) => {
                self.advance();
                Some(ExpressionKind::Literal(Literal::Bool(b)))
            }
            TokenKind::NothingLiteral => {
                self.advance();
                Some(ExpressionKind::Literal(Literal::Nothing))
            }
            TokenKind::Identifier(name) => {
                // Special case for built-in functions that are keywords in the grammar
                if name == "convert" {
                    let start_span = self.current_span();
                    self.advance(); // consume 'convert'
                    return self.parse_convert_expression(start_span, errors);
                }
                if name == "parse" {
                    let start_span = self.current_span();
                    self.advance(); // consume 'parse'
                    return self.parse_parse_expression(start_span, errors);
                }
                if name == "collect" {
                    let start_span = self.current_span();
                    self.advance(); // consume 'collect'
                    return self.parse_collect_expression(start_span, errors);
                }
                if name == "importPy" {
                    let start_span = self.current_span();
                    self.advance(); // consume 'importPy'
                    return self.parse_import_py_expression(start_span, errors);
                }
                self.advance();
                Some(ExpressionKind::Identifier(name))
            }
            TokenKind::Switch => return self.parse_switch_expression(errors),
            TokenKind::Try => {
                self.advance();
                let expr = self.parse_expression(errors)?;
                Some(ExpressionKind::Try(Box::new(expr)))
            }
            TokenKind::Type(name) => {
                self.advance();
                let generic_args = if self.current_kind() == &TokenKind::LAngle {
                    self.parse_generic_argument_list(errors)?
                } else {
                    Vec::new()
                };
                match self.current_kind() {
                    TokenKind::LParen => {
                        let (args, _) = self.parse_argument_list(errors)?;
                        Some(ExpressionKind::StructInstantiation(name, generic_args, args))
                    }
                    TokenKind::Dot => {
                        self.advance();
                        let (variant_name, _) = match self.current_kind().clone() {
                            TokenKind::Identifier(_) => self.expect_identifier(errors),
                            TokenKind::Type(_) => self.expect_type_identifier(errors),
                            _ => {
                                errors.push(ParserError::ExpectedVariantName {
                                    span: self.current_span(),
                                });
                                None
                            }
                        }?;

                        if *self.current_kind() == TokenKind::LParen {
                            self.advance();
                            let payload = self.parse_expression(errors)?;
                            self.eat(&TokenKind::RParen, errors)?;
                            Some(ExpressionKind::TaggedUnionInstantiation {
                                enum_name: name,
                                variant_name,
                                payload: Some(Box::new(payload)),
                            })
                        } else {
                            Some(ExpressionKind::EnumVariantAccess {
                                enum_name: name,
                                variant_name,
                            })
                        }
                    }
                    _ => {
                        return Some(Expression {
                            kind: ExpressionKind::Identifier(name),
                            span: token.span.clone(),
                        });
                    }
                }
            }
            TokenKind::LBracket => {
                self.advance();
                let mut elements = Vec::new();
                if *self.current_kind() != TokenKind::RBracket {
                    loop {
                        self.skip_newlines();
                        elements.push(self.parse_expression(errors)?);
                        self.skip_newlines();
                        if *self.current_kind() == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.skip_newlines();
                self.eat(&TokenKind::RBracket, errors)?;
                Some(ExpressionKind::ArrayLiteral(elements))
            }
            TokenKind::LParen => {
                self.advance();
                let inner_expr = self.parse_expression(errors)?;
                if *self.current_kind() == TokenKind::Comma {
                    let mut elements = vec![inner_expr];
                    while *self.current_kind() == TokenKind::Comma {
                        self.advance();
                        elements.push(self.parse_expression(errors)?);
                    }
                    self.eat(&TokenKind::RParen, errors)?;
                    Some(ExpressionKind::Tuple(elements))
                } else {
                    self.eat(&TokenKind::RParen, errors)?;
                    return Some(inner_expr);
                }
            }
            TokenKind::Ampersand => {
                self.advance();
                let expr = self.parse_primary(errors)?;
                Some(ExpressionKind::AddressOf(Box::new(expr)))
            }
            TokenKind::Asterisk => {
                self.advance();
                let expr = self.parse_primary(errors)?;
                Some(ExpressionKind::Dereference(Box::new(expr)))
            }
            _ => {
                errors.push(ParserError::UnexpectedToken {
                    expected: "expression".to_string(),
                    found: format!("{:?}", self.current_kind()),
                    span: self.current_span(),
                });
                None
            }
        }?;
        let end_span = self.current_span();
        Some(Expression {
            kind,
            span: token.span.merge(&end_span),
        })
    }

    /// Parses a list of generic type arguments, e.g., `<Type1, Type2>`.
    /// This now correctly handles nested generics like `<Vector<String>>` by using the
    /// centralized `parse_generic_list` helper.
    fn parse_generic_argument_list(&mut self, errors: &mut Vec<ParserError>) -> Option<Vec<Type>> {
        self.parse_generic_list(Self::parse_type, errors)
    }

    /// Parses a `switch` expression. It handles two forms for each branch:
    /// 1. A single `yield <expression>` followed by `end`.
    /// 2. A block of statements terminated by `end`.
    /// It now also handles destructuring patterns like `MyEnum.Variant(value)`.
    fn parse_switch_expression(&mut self, errors: &mut Vec<ParserError>) -> Option<Expression> {
        let start_token = self.eat(&TokenKind::Switch, errors)?;
        let expression = self.parse_expression(errors)?;
        self.skip_newlines();

        let mut cases = Vec::new();
        while *self.current_kind() == TokenKind::Case {
            self.eat(&TokenKind::Case, errors)?;
            self.eat(&TokenKind::Of, errors)?;

            let mut patterns = Vec::new();
            loop {
                let pattern_start_span = self.current_span();

                // Lookahead to see if it's `TypeName.VariantName`
                let is_variant_path =
                    if let (TokenKind::Type(_), Some(peek)) = (self.current_kind(), self.tokens.get(self.pos + 1))
                    {
                        peek.kind == TokenKind::Dot
                    } else {
                        false
                    };

                if is_variant_path {
                    let (enum_name, _) = self.expect_type_identifier(errors)?;
                    self.eat(&TokenKind::Dot, errors)?;
                    let (variant_name, _) = self.expect_type_identifier(errors)?;
                    let after_variant_span = self.current_span();

                    if *self.current_kind() == TokenKind::LParen {
                        // This is a destructuring pattern: `Type.Variant(...)`
                        self.advance();
                        let (var_name, _) = self.expect_identifier(errors)?;
                        self.eat(&TokenKind::RParen, errors)?;
                        let pattern_end_span = self.current_span();
                        patterns.push(Pattern::Destructure {
                            variant_path: (enum_name, variant_name),
                            binding: Some(var_name),
                            span: pattern_start_span.merge(&pattern_end_span),
                        });
                    } else {
                        // This is a simple variant access: `Type.Variant`
                        let expr = Expression {
                            kind: ExpressionKind::EnumVariantAccess { enum_name, variant_name },
                            span: pattern_start_span.merge(&after_variant_span),
                        };
                        patterns.push(Pattern::Literal(expr));
                    }
                } else {
                    // It's another kind of literal pattern (number, bool, etc.)
                    let expr = self.parse_expression(errors)?;
                    patterns.push(Pattern::Literal(expr));
                }

                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }

            self.skip_newlines();
            self.eat(&TokenKind::Do, errors)?;
            self.skip_newlines();

            let body = if *self.current_kind() == TokenKind::Yield {
                let start_span = self.current_span();
                self.advance();
                let expr = self.parse_expression(errors)?;
                let span = start_span.merge(&expr.span);
                vec![Statement {
                    kind: StatementKind::Yield(expr),
                    span,
                }]
            } else {
                self.parse_block(&[TokenKind::End], true, errors)
            };

            self.eat(&TokenKind::End, errors)?;
            self.skip_newlines();
            cases.push(SwitchCase { patterns, body });
        }

        let mut else_block = None;
        if *self.current_kind() == TokenKind::Else {
            self.advance();
            self.skip_newlines();

            let body = if *self.current_kind() == TokenKind::Yield {
                let start_span = self.current_span();
                self.advance();
                let expr = self.parse_expression(errors)?;
                let span = start_span.merge(&expr.span);
                vec![Statement {
                    kind: StatementKind::Yield(expr),
                    span,
                }]
            } else {
                self.parse_block(&[TokenKind::End], false, errors)
            };

            self.eat(&TokenKind::End, errors)?;
            self.skip_newlines();
            else_block = Some(body);
        }

        let end_token = self.eat(&TokenKind::End, errors)?;
        let end_span = end_token.span;
        Some(Expression {
            kind: ExpressionKind::Switch {
                expression: Box::new(expression),
                cases,
                else_block,
            },
            span: start_token.span.merge(&end_span),
        })
    }

    /// Parses the special `convert(expression, Type)` built-in function call.
    fn parse_convert_expression(&mut self, start_span: Span, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.eat(&TokenKind::LParen, errors)?;
        let expr = self.parse_expression(errors)?;
        self.eat(&TokenKind::Comma, errors)?;
        let target_type = self.parse_type(errors)?;
        let rparen = self.eat(&TokenKind::RParen, errors)?;

        Some(Expression {
            kind: ExpressionKind::Convert {
                expr: Box::new(expr),
                target_type,
            },
            span: start_span.merge(&rparen.span),
        })
    }

    /// Parses the special `parse(Type, expression)` built-in function call.
    fn parse_parse_expression(&mut self, start_span: Span, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.eat(&TokenKind::LParen, errors)?;
        let target_type = self.parse_type(errors)?;
        self.eat(&TokenKind::Comma, errors)?;
        let expr = self.parse_expression(errors)?;
        let rparen = self.eat(&TokenKind::RParen, errors)?;

        Some(Expression {
            kind: ExpressionKind::Parse {
                target_type,
                expr: Box::new(expr),
            },
            span: start_span.merge(&rparen.span),
        })
    }

    /// Parses the special `collect(start : stop)` or `collect(start : step : stop)` built-in function call.
    fn parse_collect_expression(&mut self, start_span: Span, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.eat(&TokenKind::LParen, errors)?;
        let start_expr = self.parse_expression(errors)?;
        self.eat(&TokenKind::Colon, errors)?;
        let second_expr = self.parse_expression(errors)?;

        let args = if *self.current_kind() == TokenKind::Colon {
            // 3-argument form: start : step : stop
            self.eat(&TokenKind::Colon, errors)?;
            let stop_expr = self.parse_expression(errors)?;
            vec![start_expr, second_expr, stop_expr]
        } else {
            // 2-argument form: start : stop
            vec![start_expr, second_expr]
        };

        let rparen = self.eat(&TokenKind::RParen, errors)?;
        Some(Expression {
            kind: ExpressionKind::FunctionCall {
                name: "collect".to_string(),
                generic_args: vec![],
                args,
            },
            span: start_span.merge(&rparen.span),
        })
    }

    /// Parses the special `importPy("module")` built-in function call.
    fn parse_import_py_expression(&mut self, start_span: Span, errors: &mut Vec<ParserError>) -> Option<Expression> {
        self.eat(&TokenKind::LParen, errors)?;
        let arg = self.parse_expression(errors)?;
        let rparen = self.eat(&TokenKind::RParen, errors)?;
        Some(Expression {
            kind: ExpressionKind::FunctionCall {
                name: "importPy".to_string(),
                generic_args: vec![],
                args: vec![arg],
            },
            span: start_span.merge(&rparen.span),
        })
    }

    /// Parses the argument list of a function or method call.
    pub fn parse_function_call_args(&mut self, errors: &mut Vec<ParserError>) -> Option<(Vec<Expression>, Span)> {
        let lparen = self.eat(&TokenKind::LParen, errors)?;
        let mut args = Vec::new();

        self.skip_newlines();

        if *self.current_kind() != TokenKind::RParen {
            loop {
                args.push(self.parse_expression(errors)?);
                self.skip_newlines();
                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                    self.skip_newlines();
                } else {
                    break;
                }
            }
        }
        let rparen = self.eat(&TokenKind::RParen, errors)?;
        Some((args, lparen.span.merge(&rparen.span)))
    }

    /// Parses an argument list for a struct instantiation expression.
    /// This supports both positional (`value`) and named (`field: value`) arguments.
    /// A rule is enforced that all positional arguments must come before any named arguments.
    pub fn parse_argument_list(&mut self, errors: &mut Vec<ParserError>) -> Option<(Vec<Arg>, Span)> {
        let lparen = self.eat(&TokenKind::LParen, errors)?;
        let mut args = Vec::new();
        let mut has_seen_named = false;

        if *self.current_kind() != TokenKind::RParen {
            loop {
                let is_named = if let (TokenKind::Identifier(_), Some(peek)) = (self.current_kind(), self.tokens.get(self.pos + 1)) {
                    peek.kind == TokenKind::Colon
                } else {
                    false
                };

                if is_named {
                    has_seen_named = true;
                    let (name, _) = self.expect_identifier(errors)?;
                    self.eat(&TokenKind::Colon, errors)?;
                    let value = self.parse_expression(errors)?;
                    args.push(Arg::Named(name, value));
                } else {
                    if has_seen_named {
                        errors.push(ParserError::UnexpectedToken {
                            expected: "a named argument (name: value)".to_string(),
                            found: "a positional argument after a named one".to_string(),
                            span: self.current_span(),
                        });
                        return None;
                    }
                    let value = self.parse_expression(errors)?;
                    args.push(Arg::Positional(value));
                }

                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }

        let rparen = self.eat(&TokenKind::RParen, errors)?;
        Some((args, lparen.span.merge(&rparen.span)))
    }

    /// Parses an argument list for a dictionary initializer.
    /// This supports only key-value pair arguments (`key => value`) and correctly handles
    /// newlines to allow for multi-line initializers.
    pub fn parse_dictionary_argument_list(&mut self, errors: &mut Vec<ParserError>) -> Option<(Vec<Arg>, Span)> {
        let lparen = self.eat(&TokenKind::LParen, errors)?;
        let mut args = Vec::new();

        if *self.current_kind() != TokenKind::RParen {
            loop {
                self.skip_newlines();

                // This handles the case of a trailing comma followed by a newline and the closing paren.
                if *self.current_kind() == TokenKind::RParen {
                    break;
                }

                let key_expr = self.parse_expression(errors)?;
                self.eat(&TokenKind::FatArrow, errors)?;
                let value_expr = self.parse_expression(errors)?;
                args.push(Arg::KeyValuePair(key_expr, value_expr));

                self.skip_newlines();

                if *self.current_kind() == TokenKind::Comma {
                    self.advance(); // Consume the comma
                } else {
                    break; // No comma means this is the last item.
                }
            }
        }

        self.skip_newlines();
        let rparen = self.eat(&TokenKind::RParen, errors)?;
        Some((args, lparen.span.merge(&rparen.span)))
    }
}