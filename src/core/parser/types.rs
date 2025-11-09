// src/core/parser/types.rs
//! Contains the parsing logic for type annotations in the Jophet language.
//!
//! This module provides the `parse_type` method, which is responsible for parsing
//! the various forms a type can take, including simple names, generic types like
//! `Vector<T>`, tuple types with their special syntax `Tuple<(T1, T2)>`,
//! references (`&T`), fallible types (`T?`), and the explicit closure type syntax
//! `Closure<(T1, T2): TReturn>`. All parsing functions have been updated to collect
//! errors and return `Option<T>` on failure.

use super::Parser;
use crate::core::ast::untyped::*;
use crate::core::ast::TokenKind;
use crate::diagnostics::errors::ParserError;

impl Parser {
    /// Parses a type annotation.
    ///
    /// This function handles the recursive nature of type definitions. It can parse:
    /// - Simple types: `MyStruct`
    /// - Generic types: `Vector<String>`, `Array<Int64, 4>`
    /// - Nested generic types: `Vector<Vector<Int64>>`
    /// - Tuple types with special syntax: `Tuple<(String, Int64)>`
    /// - Closure types: `Closure<(String, Int64): Bool>`
    /// - References: `&MyStruct`, `&mutable MyStruct`
    /// - Raw pointers: `raw *MyStruct`
    /// - Fallible types: `MyType?`
    /// These can all be combined, e.g., `&Vector<Closure<(): Nothing>>`.
    pub fn parse_type(&mut self, errors: &mut Vec<ParserError>) -> Option<Type> {
        // First, check for reference markers or the `raw` keyword.
        let mut parsed_type = if *self.current_kind() == TokenKind::Ampersand {
            self.advance();
            if *self.current_kind() == TokenKind::Mutable {
                self.advance();
                Some(Type::MutableReference(Box::new(self.parse_type(errors)?)))
            } else {
                Some(Type::Reference(Box::new(self.parse_type(errors)?)))
            }
        } else if *self.current_kind() == TokenKind::Raw {
            self.advance(); // consume `raw`
            self.eat(&TokenKind::Asterisk, errors)?;
            Some(Type::RawPointer(Box::new(self.parse_type(errors)?)))
        } else {
            // If not a reference or raw pointer, parse a simple or generic type name.
            let (base_type_name, _) = self.expect_type_identifier(errors)?;
            // Check for generic parameters `<...>`.
            if *self.current_kind() == TokenKind::LAngle {
                self.advance();
                let mut generic_params = Vec::new();

                // Special handling for Array<Type, Size>
                if base_type_name == "Array" {
                    let member_type = self.parse_type(errors)?;
                    self.eat(&TokenKind::Comma, errors)?;
                    let size = if let TokenKind::IntLiteral(val) = *self.current_kind() {
                        self.advance();
                        val
                    } else {
                        errors.push(ParserError::UnexpectedToken {
                            expected: "an integer literal for array size".to_string(),
                            found: format!("{:?}", self.current_kind()),
                            span: self.current_span(),
                        });
                        return None;
                    };
                    self.eat_closing_rangle(errors)?;
                    return Some(Type::Array(Box::new(member_type), size));
                }

                // Special handling for Tuple<(T1, T2, ...)> syntax
                if base_type_name == "Tuple" && *self.current_kind() == TokenKind::LParen {
                    self.advance(); // consume `(`
                    while *self.current_kind() != TokenKind::RParen {
                        generic_params.push(self.parse_type(errors)?);
                        if *self.current_kind() == TokenKind::Comma {
                            self.advance();
                        } else {
                            break; // Allow a trailing comma
                        }
                    }
                    self.eat(&TokenKind::RParen, errors)?; // consume `)`
                } else if base_type_name == "Closure" && *self.current_kind() == TokenKind::LParen {
                    // Special handling for Closure<(T1, T2): TReturn> syntax
                    self.advance(); // consume `(`
                    let mut closure_params = Vec::new();
                    if *self.current_kind() != TokenKind::RParen {
                        loop {
                            closure_params.push(self.parse_type(errors)?);
                            if *self.current_kind() == TokenKind::Comma {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&TokenKind::RParen, errors)?; // consume `)`
                    self.eat(&TokenKind::Colon, errors)?;
                    let ret_type = self.parse_type(errors)?;
                    self.eat_closing_rangle(errors)?;
                    return Some(Type::Closure {
                        params: closure_params,
                        ret: Box::new(ret_type),
                    });
                } else {
                    // Standard generic parsing for types like Vector<T>
                    while *self.current_kind() != TokenKind::RAngle && *self.current_kind() != TokenKind::GreaterGreater {
                        generic_params.push(self.parse_type(errors)?);
                        if *self.current_kind() == TokenKind::Comma {
                            self.advance();
                        } else {
                            break; // Correctly handles single-parameter generics like Vector<String>
                        }
                    }
                }

                self.eat_closing_rangle(errors)?;
                Some(Type::Generic(base_type_name, generic_params))
            } else {
                Some(Type::Simple(base_type_name))
            }
        }?;

        // After parsing the base type, check for a `?` suffix for fallible types.
        if *self.current_kind() == TokenKind::Question {
            self.advance();
            parsed_type = Type::Fallible(Box::new(parsed_type));
        }

        Some(parsed_type)
    }

    /// Parses a "primary" type, which is a type without any `?` suffix.
    /// This is a helper for parsing bounds to avoid infinite recursion.
    fn parse_primary_type(&mut self, errors: &mut Vec<ParserError>) -> Option<Type> {
        if *self.current_kind() == TokenKind::Ampersand {
            self.advance();
            if *self.current_kind() == TokenKind::Mutable {
                self.advance();
                Some(Type::MutableReference(Box::new(self.parse_type(errors)?)))
            } else {
                Some(Type::Reference(Box::new(self.parse_type(errors)?)))
            }
        } else if *self.current_kind() == TokenKind::Raw {
            self.advance();
            self.eat(&TokenKind::Asterisk, errors)?;
            Some(Type::RawPointer(Box::new(self.parse_type(errors)?)))
        } else {
            let (base_type_name, _) = self.expect_type_identifier(errors)?;
            if *self.current_kind() == TokenKind::LAngle {
                self.advance();
                let mut generic_params = Vec::new();

                if base_type_name == "Array" {
                    let member_type = self.parse_type(errors)?;
                    self.eat(&TokenKind::Comma, errors)?;
                    let size = if let TokenKind::IntLiteral(val) = *self.current_kind() {
                        self.advance();
                        val
                    } else {
                        errors.push(ParserError::UnexpectedToken {
                            expected: "an integer literal for array size".to_string(),
                            found: format!("{:?}", self.current_kind()),
                            span: self.current_span(),
                        });
                        return None;
                    };
                    self.eat_closing_rangle(errors)?;
                    return Some(Type::Array(Box::new(member_type), size));
                }

                if base_type_name == "Tuple" && *self.current_kind() == TokenKind::LParen {
                    self.advance();
                    while *self.current_kind() != TokenKind::RParen {
                        generic_params.push(self.parse_type(errors)?);
                        if *self.current_kind() == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                    self.eat(&TokenKind::RParen, errors)?;
                } else if base_type_name == "Closure" && *self.current_kind() == TokenKind::LParen {
                    self.advance(); // consume `(`
                    let mut closure_params = Vec::new();
                    if *self.current_kind() != TokenKind::RParen {
                        loop {
                            closure_params.push(self.parse_type(errors)?);
                            if *self.current_kind() == TokenKind::Comma {
                                self.advance();
                            } else {
                                break;
                            }
                        }
                    }
                    self.eat(&TokenKind::RParen, errors)?; // consume `)`
                    self.eat(&TokenKind::Colon, errors)?;
                    let ret_type = self.parse_type(errors)?;
                    self.eat_closing_rangle(errors)?;
                    return Some(Type::Closure {
                        params: closure_params,
                        ret: Box::new(ret_type),
                    });
                } else {
                    while *self.current_kind() != TokenKind::RAngle && *self.current_kind() != TokenKind::GreaterGreater {
                        generic_params.push(self.parse_type(errors)?);
                        if *self.current_kind() == TokenKind::Comma {
                            self.advance();
                        } else {
                            break;
                        }
                    }
                }
                self.eat_closing_rangle(errors)?;
                Some(Type::Generic(base_type_name, generic_params))
            } else {
                Some(Type::Simple(base_type_name))
            }
        }
    }
}