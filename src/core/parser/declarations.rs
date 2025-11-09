// src/core/parser/declarations.rs
//! Contains the parsing logic for declarations in the Jophet language.
//!
//! This module implements the parsing methods for top-level (and sometimes nested)
//! declarative constructs like imports, structs, enums, functions, and variable
//! declarations. Each method is responsible for consuming a sequence of tokens
//! and producing a corresponding node for the Untyped AST. It now accepts an
//! optional doc comment string to associate with the parsed declaration. All parsing
//! functions have been updated to collect errors and return `Option<T>` on failure.

use super::Parser;
use crate::core::ast::untyped::*;
use crate::core::ast::TokenKind;
use crate::diagnostics::errors::ParserError;

impl Parser {
    /// Parses an `import` statement. It now accepts a dot-separated path of identifiers.
    /// Example: `import my_module` or `import my_module.my_func`
    pub fn parse_import_statement(&mut self, errors: &mut Vec<ParserError>) -> Option<StatementKind> {
        self.eat(&TokenKind::Import, errors)?;
        
        let mut path = Vec::new();
        
        // The first segment can be an identifier or a type name (for built-ins like Error).
        let (first_segment, _) = self.expect_path_segment(errors)?;
        path.push(first_segment);

        // Loop to consume subsequent `.member` parts.
        while *self.current_kind() == TokenKind::Dot {
            self.advance(); // consume '.'
            let (next_segment, _) = self.expect_path_segment(errors)?;
            path.push(next_segment);
        }
        
        Some(StatementKind::Import { path })
    }

    /// Parses a single generic parameter, including its optional trait bounds.
    /// Example: `T` or `T: Bound1, Bound2`
    fn parse_generic_param(&mut self, errors: &mut Vec<ParserError>) -> Option<GenericParam> {
        let (name, _) = self.expect_type_identifier(errors)?;
        let mut bounds = Vec::new();
        if *self.current_kind() == TokenKind::Colon {
            self.advance(); // consume ':'
            loop {
                bounds.push(self.parse_type(errors)?);
                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        Some(GenericParam { name, bounds })
    }

    /// Parses a list of generic parameters, e.g., `<T: Bound, U>`.
    /// This now correctly handles nested generics like `<T: Trait<U>>` by using the
    /// `eat_closing_rangle` helper to disambiguate `>>`.
    fn parse_optional_generic_params(&mut self, errors: &mut Vec<ParserError>) -> Option<Vec<GenericParam>> {
        let mut generic_params = Vec::new();
        if *self.current_kind() == TokenKind::LAngle {
            self.advance(); // Consume '<'
            if *self.current_kind() != TokenKind::RAngle && *self.current_kind() != TokenKind::GreaterGreater {
                loop {
                    generic_params.push(self.parse_generic_param(errors)?);
                    if *self.current_kind() == TokenKind::Comma {
                        self.advance();
                    } else {
                        break;
                    }
                }
            }
            self.eat_closing_rangle(errors)?; // Consume '>' using the new helper
        }
        Some(generic_params)
    }

    /// Parses a `struct` definition. Fields can be separated by newlines or optional commas.
    /// Example:
    /// `/// A simple struct.
    ///  public struct MyStruct<T>
    ///    /// The horizontal coordinate.
    ///    x: Int64, public y: T
    ///  end`
    pub fn parse_struct_def(
        &mut self,
        doc_comment: Option<String>,
        is_public: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        self.eat(&TokenKind::Struct, errors)?;
        let (name, _) = self.expect_type_identifier(errors)?;
        let generic_params = self.parse_optional_generic_params(errors)?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while *self.current_kind() != TokenKind::End {
            let field_doc_comment = self.parse_optional_doc_comment(errors).flatten();
            let field_is_public = if *self.current_kind() == TokenKind::Public {
                self.advance();
                true
            } else {
                false
            };

            let (field_name, _) = self.expect_identifier(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            let field_type = self.parse_type(errors)?;
            fields.push((field_name, field_type, field_is_public, field_doc_comment));

            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::StructDef(StructDef {
            is_public,
            name,
            doc_comment,
            generic_params,
            fields,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses an `enum` definition. Variants must be `PascalCase` and can be comma-separated.
    /// Each variant can optionally be assigned an explicit integer value and can have a doc comment.
    /// Example:
    /// `/// An enumeration of states.
    ///  enum Status
    ///    Ready,
    ///    /// The process is running.
    ///    InProgress = 5,
    ///    Done // will be assigned 6
    ///  end`
    pub fn parse_enum_def(
        &mut self,
        doc_comment: Option<String>,
        is_public: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        self.eat(&TokenKind::Enum, errors)?;
        let (name, _) = self.expect_type_identifier(errors)?;
        self.skip_newlines();
        let mut members = Vec::new();
        while *self.current_kind() != TokenKind::End {
            let member_doc_comment = self.parse_optional_doc_comment(errors).flatten();
            let (member_name, _) = self.expect_type_identifier(errors)?;

            // Check for an optional explicit value assignment.
            let value = if *self.current_kind() == TokenKind::Equal {
                self.advance();
                if let TokenKind::IntLiteral(val) = self.current_kind() {
                    let assigned_val = *val;
                    self.advance();
                    Some(assigned_val)
                } else {
                    errors.push(ParserError::UnexpectedToken {
                        expected: "integer literal for enum value".to_string(),
                        found: format!("{:?}", self.current_kind()),
                        span: self.current_span(),
                    });
                    return None;
                }
            } else {
                None
            };

            members.push((member_name, value, member_doc_comment));

            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::EnumDef(EnumDef {
            is_public,
            name,
            doc_comment,
            members,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses a `union` definition. Fields can be separated by newlines or optional commas,
    /// and each field can now have a doc comment.
    /// Example:
    /// `/// Can hold an Int64 or a Float64.
    ///  union IntOrFloat
    ///    /// The integer field.
    ///    i: Int64,
    ///    /// The float field.
    ///    f: Float64
    ///  end`
    pub fn parse_union_def(
        &mut self,
        doc_comment: Option<String>,
        is_public: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        self.eat(&TokenKind::Union, errors)?;
        let (name, _) = self.expect_type_identifier(errors)?;
        self.skip_newlines();
        let mut fields = Vec::new();
        while *self.current_kind() != TokenKind::End {
            let field_doc_comment = self.parse_optional_doc_comment(errors).flatten();
            let (field_name, _) = self.expect_identifier(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            let field_type = self.parse_type(errors)?;
            fields.push((field_name, field_type, field_doc_comment));

            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::UnionDef(UnionDef {
            is_public,
            name,
            doc_comment,
            fields,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses a `tagged union` definition. Variants can be separated by newlines or optional commas.
    /// Example:
    /// `/// Represents a message in a system.
    ///  tagged union Message<T>
    ///    /// A variant with no associated data.
    ///    Quit, Write(String), Move(T)
    ///  end`
    pub fn parse_tagged_union_def(
        &mut self,
        doc_comment: Option<String>,
        is_public: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        self.eat(&TokenKind::TaggedUnion, errors)?;
        let (name, _) = self.expect_type_identifier(errors)?;
        let generic_params = self.parse_optional_generic_params(errors)?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while *self.current_kind() != TokenKind::End {
            let variant_doc_comment = self.parse_optional_doc_comment(errors).flatten();
            let (variant_name, _) = self.expect_type_identifier(errors)?;
            let payload = if *self.current_kind() == TokenKind::LParen {
                self.advance();
                let payload_type = self.parse_type(errors)?;
                self.eat(&TokenKind::RParen, errors)?;
                Some(payload_type)
            } else {
                None
            };
            variants.push(TaggedUnionVariant {
                name: variant_name,
                doc_comment: variant_doc_comment,
                payload,
            });

            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::TaggedUnionDef(TaggedUnionDef {
            is_public,
            name,
            doc_comment,
            generic_params,
            variants,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses an `error` definition. Variants can be separated by newlines or optional commas.
    /// Syntactically identical to a `tagged union`.
    /// Example:
    /// `/// An error that can occur during file operations.
    ///  error FileError
    ///    NotFound, PermissionDenied
    ///  end`
    pub fn parse_error_def(
        &mut self,
        doc_comment: Option<String>,
        is_public: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        self.eat(&TokenKind::Error, errors)?;
        let (name, _) = self.expect_type_identifier(errors)?;
        self.skip_newlines();
        let mut variants = Vec::new();
        while *self.current_kind() != TokenKind::End {
            let variant_doc_comment = self.parse_optional_doc_comment(errors).flatten();
            let (variant_name, _) = self.expect_type_identifier(errors)?;
            let payload = if *self.current_kind() == TokenKind::LParen {
                self.advance();
                let payload_type = self.parse_type(errors)?;
                self.eat(&TokenKind::RParen, errors)?;
                Some(payload_type)
            } else {
                None
            };
            variants.push(TaggedUnionVariant {
                name: variant_name,
                doc_comment: variant_doc_comment,
                payload,
            });

            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::ErrorDef(ErrorDef {
            is_public,
            name,
            doc_comment,
            variants,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses a `trait` definition, which contains only function signatures.
    /// Example:
    /// `/// A trait for types that can be printed.
    ///  public trait Printable<T>
    ///    /// Converts the object to a string representation.
    ///    function to_string(self): String
    ///    function format(self, formatter: T)
    ///  end`
    pub fn parse_trait_def(
        &mut self,
        doc_comment: Option<String>,
        is_public: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<StatementKind> {
        self.eat(&TokenKind::Trait, errors)?;
        let (name, _) = self.expect_type_identifier(errors)?;
        let generic_params = self.parse_optional_generic_params(errors)?;
        self.skip_newlines();
        let mut methods = Vec::new();
        while *self.current_kind() != TokenKind::End {
            methods.push(self.parse_function_signature(errors)?);
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::TraitDef(TraitDef {
            is_public,
            name,
            doc_comment,
            generic_params,
            methods,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses an `implement` block, which contains method definitions for a struct or trait.
    /// Example:
    /// `/// Implements methods for the `Rectangle` struct.
    ///  implement MyStruct
    ///    ...
    ///  end`
    /// `implement Printable for MyStruct
    ///    ...
    ///  end`
    pub fn parse_implement_block(&mut self, doc_comment: Option<String>, errors: &mut Vec<ParserError>) -> Option<StatementKind> {
        self.eat(&TokenKind::Implement, errors)?;
        let first_type = self.parse_type(errors)?;
        let (target_type, trait_type) = if *self.current_kind() == TokenKind::For {
            self.advance(); // consume 'for'
            let second_type = self.parse_type(errors)?;
            (second_type, Some(first_type))
        } else {
            (first_type, None)
        };

        self.skip_newlines();
        let mut methods = Vec::new();
        while *self.current_kind() != TokenKind::End {
            let method_doc_comment = self.parse_optional_doc_comment(errors).flatten();
            let is_public = if *self.current_kind() == TokenKind::Public {
                self.advance();
                true
            } else {
                false
            };
            // `is_const` is false for methods in an impl block.
            methods.push(self.parse_function_like(method_doc_comment, true, is_public, false, errors)?);
            self.skip_newlines();
        }
        self.eat(&TokenKind::End, errors)?;
        Some(StatementKind::ImplementBlock(ImplementBlock {
            doc_comment,
            target_type,
            trait_type,
            methods,
            module_path: self.current_file.clone(),
        }))
    }

    /// Parses a function signature (without a body), for use in `trait` definitions.
    pub fn parse_function_signature(&mut self, errors: &mut Vec<ParserError>) -> Option<FunctionDecl> {
        let doc_comment = self.parse_optional_doc_comment(errors).flatten();
        let is_public = if *self.current_kind() == TokenKind::Public {
            self.advance();
            true
        } else {
            false
        };
        self.eat(&TokenKind::Function, errors)?;
        let (name, _) = self.expect_identifier(errors)?;
        let generic_params = self.parse_optional_generic_params(errors)?;
        self.eat(&TokenKind::LParen, errors)?;
        let mut params = Vec::new();
        let mut has_self = false;
        if *self.current_kind() == TokenKind::Identifier("self".to_string()) {
            has_self = true;
            self.advance();
            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        while *self.current_kind() != TokenKind::RParen {
            let (param_name, _) = self.expect_identifier(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            params.push((param_name, self.parse_type(errors)?));
            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        self.eat(&TokenKind::RParen, errors)?;

        // Return type is now optional for trait method signatures as well.
        let return_type = if *self.current_kind() == TokenKind::Colon {
            self.advance();
            Some(self.parse_type(errors)?)
        } else {
            None
        };

        Some(FunctionDecl {
            is_public,
            is_const: false, // Trait method signatures cannot be `const`
            name,
            doc_comment,
            generic_params,
            has_self,
            params,
            return_type,
            body: Vec::new(), // Signatures have no body.
            module_path: self.current_file.clone(),
        })
    }

    /// Parses a `function` definition, which can be a standalone function, a method,
    /// or an anonymous function (closure).
    ///
    /// # Arguments
    /// * `doc_comment` - The documentation comment string, if any.
    /// * `is_method` - `true` if parsing inside an `implement` block. This enables
    ///   parsing of the special `self` parameter.
    /// * `is_public` - `true` if the function was preceded by the `public` keyword.
    /// * `is_const` - `true` if the function was preceded by the `const` keyword.
    pub fn parse_function_like(
        &mut self,
        doc_comment: Option<String>,
        is_method: bool,
        is_public: bool,
        is_const: bool,
        errors: &mut Vec<ParserError>,
    ) -> Option<FunctionDecl> {
        self.eat(&TokenKind::Function, errors)?;
        // A closure is an expression, so it cannot have a name here.
        // A function declaration (statement) must have a name.
        let name = if *self.current_kind() != TokenKind::LParen {
            let (ident, _) = self.expect_identifier(errors)?;
            ident
        } else {
            "".to_string() // Anonymous function
        };

        let generic_params = self.parse_optional_generic_params(errors)?;
        self.eat(&TokenKind::LParen, errors)?;
        let mut params = Vec::new();
        let mut has_self = false;
        // Check for `self` as the first parameter in a method.
        if is_method && *self.current_kind() == TokenKind::Identifier("self".to_string()) {
            has_self = true;
            self.advance();
            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        // Parse the parameter list.
        while *self.current_kind() != TokenKind::RParen {
            let (param_name, _) = self.expect_identifier(errors)?;
            self.eat(&TokenKind::Colon, errors)?;
            let param_type = self.parse_type(errors)?;
            params.push((param_name, param_type));

            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            }
        }
        self.eat(&TokenKind::RParen, errors)?;

        // The return type is now optional.
        let return_type = if *self.current_kind() == TokenKind::Colon {
            self.advance();
            Some(self.parse_type(errors)?)
        } else {
            None
        };

        self.skip_newlines();
        // Parse the function body as a block of statements, terminated by `end`.
        let body = self.parse_block(&[TokenKind::End], false, errors);
        self.eat(&TokenKind::End, errors)?;
        Some(FunctionDecl {
            is_public,
            is_const,
            name,
            doc_comment,
            generic_params,
            has_self,
            params,
            return_type,
            body,
            module_path: self.current_file.clone(),
        })
    }

    fn parse_declaration_pattern(&mut self, errors: &mut Vec<ParserError>) -> Option<DeclarationPattern> {
        let (pattern, open_delim, close_delim) = match self.current_kind() {
            TokenKind::LParen => (DeclarationPattern::Tuple(vec![]), TokenKind::LParen, TokenKind::RParen),
            TokenKind::LBracket => (DeclarationPattern::Array(vec![]), TokenKind::LBracket, TokenKind::RBracket),
            _ => {
                 // It's a simple variable declaration: `x: Type`
                let (name, _) = self.expect_identifier(errors)?;
                self.eat(&TokenKind::Colon, errors)?;
                let var_type = self.parse_type(errors)?;
                return Some(DeclarationPattern::Identifier(name, var_type));
            }
        };

        self.eat(&open_delim, errors)?;
        let mut targets = Vec::new();
        let mut has_rest_pattern = false;

        loop {
            // Check for closing delimiter first.
            if std::mem::discriminant(self.current_kind()) == std::mem::discriminant(&close_delim) {
                break;
            }

            // If a rest pattern has already been seen, but we encounter another item, it's an error.
            if has_rest_pattern {
                errors.push(ParserError::SyntaxError {
                    message: "The `..` rest pattern must be the last element in a destructuring list.".to_string(),
                    span: self.current_span(),
                });
                return None;
            }

            // Explicitly check for `..` token FIRST.
            if *self.current_kind() == TokenKind::DoubleDot {
                self.advance(); // consume `..` token

                has_rest_pattern = true;
                targets.push(DestructuringTarget {
                    var_name: "..".to_string(),
                    ty: Type::Simple("Nothing".to_string()), // Placeholder, semantic analyzer ignores
                    is_mutable: false, // Cannot be mutable
                    source_field: None, // Rest patterns never have source fields
                    is_rest_pattern: true,
                });
            } else {
                // It's a regular variable or `_` skip.
                let is_mutable = if *self.current_kind() == TokenKind::Mutable {
                    self.advance();
                    true
                } else {
                    false
                };
                
                let (var_name, _name_span) = self.expect_identifier(errors)?;
                
                // Validate `_` cannot be mutable
                if var_name == "_" && is_mutable {
                    errors.push(ParserError::SyntaxError {
                        message: "The `_` discard pattern cannot be declared as `mutable`.".to_string(),
                        span: self.current_span(),
                    });
                    return None;
                }

                // A type annotation is always required for explicit variables, including `_`.
                self.eat(&TokenKind::Colon, errors)?; // This line is crucial for `_` requiring a type.
                let ty = self.parse_type(errors)?;
                
                // The source field is optional (and only valid for tuple/struct patterns).
                let source_field = if *self.current_kind() == TokenKind::Equal {
                    if matches!(pattern, DeclarationPattern::Array(_)) {
                         errors.push(ParserError::SyntaxError {
                            message: "Labeled destructuring with `=` is not supported for array patterns.".to_string(),
                            span: self.current_span(),
                        });
                        return None;
                    }
                    self.advance(); // consume '='
                    let (field_name, _) = self.expect_identifier(errors)?;
                    Some(field_name)
                } else {
                    None
                };
                
                targets.push(DestructuringTarget {
                    var_name,
                    ty,
                    is_mutable,
                    source_field,
                    is_rest_pattern: false,
                });
            }

            // Expect a comma if more elements are coming.
            if *self.current_kind() == TokenKind::Comma {
                self.advance();
            } else if std::mem::discriminant(self.current_kind()) != std::mem::discriminant(&close_delim) {
                // If not a comma and not closing delimiter, it's an unexpected token.
                errors.push(ParserError::UnexpectedToken {
                    expected: format!("`,` or `{:?}`", close_delim),
                    found: format!("{:?}", self.current_kind()),
                    span: self.current_span(),
                });
                return None;
            }
        }
        self.eat(&close_delim, errors)?;
        
        Some(match pattern {
            DeclarationPattern::Tuple(_) => DeclarationPattern::Tuple(targets),
            DeclarationPattern::Array(_) => DeclarationPattern::Array(targets),
            _ => unreachable!(),
        })
    }

    /// Looks ahead in the token stream to determine if the current position marks the
    /// start of a variable declaration (e.g., `x: Type`, `(x: Type, ...)`, or `[x: Type, ...]` ).
    pub(super) fn is_declaration(&self) -> bool {
        // Lookahead must be careful not to consume tokens.
        let mut temp_parser = self.clone();

        // A `mutable` keyword at the start of a statement always indicates a declaration.
        if *temp_parser.current_kind() == TokenKind::Mutable {
            return true;
        }

        // A `const` qualifier can also precede a declaration.
        if *temp_parser.current_kind() == TokenKind::Const {
            temp_parser.advance();
        }

        let mut dummy_errors = Vec::new();
        // Try to parse a declaration pattern. If it succeeds and is followed by `=`, it's a declaration.
        if temp_parser.parse_declaration_pattern(&mut dummy_errors).is_some() {
            if *temp_parser.current_kind() == TokenKind::Equal {
                return true;
            }
        }
        
        false
    }

    /// Parses a single variable declaration, which can now be a simple or destructuring pattern.
    pub(super) fn parse_variable_declaration(&mut self, is_mutable: bool, is_const: bool, errors: &mut Vec<ParserError>) -> Option<StatementKind> {
        let pattern = self.parse_declaration_pattern(errors)?;
        self.eat(&TokenKind::Equal, errors)?;
        let initializer = self.parse_expression(errors)?;
        Some(StatementKind::VariableDecl(VariableDecl {
            pattern,
            is_const: is_const,
            is_mutable,
            initializer,
        }))
    }
}