// src/core/parser/mod.rs
//! The parser for the Jophet language.
//!
//! This module is responsible for the second phase of compilation: syntax analysis.
//! It takes a flat sequence of `Token`s from the lexer and transforms it into an
//! Untyped Abstract Syntax Tree (`untyped::Program`), which represents the hierarchical
//! structure of the source code. The parser checks if the token stream conforms to the
//! language's grammar. Instead of failing on the first error, it collects all syntax
//! errors it can find and attempts to synchronize to continue parsing, providing a more
//! user-friendly experience by reporting multiple errors at once. It now consumes
//! `DocComment` tokens and associates them with the declarations that follow, and it also
//! parses `ModuleDocComment` tokens at the start of a file.
//!
//! The parser implemented here is a recursive descent parser. It now correctly handles
//! the `..` rest pattern in tuple destructuring declarations, explicitly as a discard
//! mechanism that cannot be bound to a variable or have a type annotation. It also
//! enforces that the `_` (discard) pattern cannot be marked as `mutable`, and
//! consistently requires a type annotation for all explicit targets, including `_`.
//!
//! Additionally, the parser accepts an optional `const` qualifier before variable
//! declarations, which is forwarded to semantic analysis for compile-time evaluation
//! checks. This does not change any other syntax or semantics.

use crate::core::ast::untyped::*;
use crate::core::ast::{Span, Token, TokenKind};
use crate::diagnostics::errors::{JophetError, ParserError};
use std::path::PathBuf;

mod declarations;
mod expressions;
mod statements;
mod types;

/// A type alias for the result of a parsing operation, which can either be a
/// successfully parsed AST node or a `ParserError`.
type ParseResult<T> = Result<T, ParserError>;

/// The public entry point to the parser.
///
/// This function creates a `Parser` instance and kicks off the parsing process.
/// It now collects all syntax errors encountered instead of failing on the first one.
///
/// # Arguments
/// * `tokens` - The `Vec<Token>` produced by the lexer.
/// * `current_file` - The path to the source file being parsed, used for metadata.
///
/// # Returns
/// A `Result` containing a tuple `(Program, Vec<ParserError>)` on success.
/// The `Program` may be incomplete if syntax errors were found. The `Vec<ParserError>`
/// contains all syntax errors that were detected.
pub fn parse(tokens: Vec<Token>, current_file: PathBuf) -> Result<(Program, Vec<ParserError>), JophetError> {
    let mut parser = Parser::new(tokens, current_file.clone());
    let mut errors = Vec::new();
    let program = parser.parse_program(&mut errors);
    Ok((program, errors))
}

/// A REPL-specific entry point to the parser.
///
/// Attempts to parse the token stream as a single expression. This is used by the
/// REPL to determine if a line of input should be evaluated and have its result printed.
/// It succeeds if the entire token stream consists of one expression followed by `Eof`.
pub fn parse_as_expression(tokens: Vec<Token>, current_file: PathBuf) -> bool {
    let mut parser = Parser::new(tokens, current_file);
    let mut errors = Vec::new();
    if parser.parse_expression(&mut errors).is_some() && errors.is_empty() {
        // Ensure there are no trailing tokens other than Eof.
        parser.current_kind() == &TokenKind::Eof
    } else {
        false
    }
}

/// A REPL-specific entry point to the parser.
///
/// Attempts to parse the token stream as a single statement. This is used by the
/// REPL to determine if a line of input is a declaration that should be saved
/// for the session.
pub fn parse_single_statement(
    tokens: Vec<Token>,
    current_file: PathBuf,
) -> Result<Statement, ParserError> {
    let mut parser = Parser::new(tokens, current_file);
    let mut errors = Vec::new();
    // For REPL, we expect a single valid statement. If errors are found, we fail.
    let stmt = parser
        .parse_statement(false, &mut errors) // `allow_yield` is false for top-level REPL input
        .ok_or_else(|| {
            errors
                .pop()
                .unwrap_or_else(|| ParserError::SyntaxError {
                    message: "Invalid statement in REPL".to_string(),
                    span: Default::default(),
                })
        })?;
    if !errors.is_empty() {
        // Return the first error if multiple were somehow generated.
        return Err(errors.remove(0));
    }
    Ok(stmt)
}

/// Holds the state of the parsing process.
#[derive(Clone)]
pub struct Parser {
    /// The stream of tokens from the lexer.
    tokens: Vec<Token>,
    /// The parser's current position in the token stream.
    pos: usize,
    /// The path to the file currently being parsed.
    current_file: PathBuf,
}

impl Parser {
    /// Creates a new `Parser`.
    pub fn new(tokens: Vec<Token>, current_file: PathBuf) -> Self {
        Parser {
            tokens,
            pos: 0,
            current_file,
        }
    }

    /// Returns a reference to the current token without consuming it.
    /// If at the end of the stream, it returns the final `Eof` token.
    fn current(&self) -> &Token {
        self.tokens
            .get(self.pos)
            .unwrap_or_else(|| &self.tokens[self.tokens.len() - 1])
    }

    /// Returns a reference to the kind of the current token.
    fn current_kind(&self) -> &TokenKind {
        &self.current().kind
    }

    /// Returns the span of the current token.
    fn current_span(&self) -> Span {
        self.current().span.clone()
    }

    /// Peeks at a token `n` positions ahead of the current one.
    fn peek_at(&self, n: usize) -> &Token {
        self.tokens
            .get(self.pos + n)
            .unwrap_or_else(|| &self.tokens[self.tokens.len() - 1])
    }

    /// Consumes the current token and advances the parser's position.
    /// Returns the token that was just consumed.
    fn advance(&mut self) -> &Token {
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        &self.tokens[self.pos - 1]
    }

    /// Consumes the current token if it matches the expected kind.
    /// If it matches, the token is returned. If not, a `ParserError` is pushed to the
    /// error collector and `None` is returned.
    fn eat(&mut self, expected: &TokenKind, errors: &mut Vec<ParserError>) -> Option<Token> {
        if std::mem::discriminant(self.current_kind()) == std::mem::discriminant(expected) {
            Some(self.advance().clone())
        } else {
            errors.push(ParserError::UnexpectedToken {
                expected: format!("{:?}", expected),
                found: format!("{:?}", self.current_kind()),
                span: self.current_span(),
            });
            None
        }
    }

    /// After a syntax error, this method attempts to find the beginning of the next statement.
    /// This allows the parser to report more than one error by skipping tokens until it finds a
    /// reasonable place to resume parsing.
    fn synchronize(&mut self) {
        self.advance(); // Consume the token that caused the error to avoid an infinite loop.

        while self.current_kind() != &TokenKind::Eof {
            // If the previous token was a newline, we are likely at the start of a new line,
            // which is a good place to resume.
            if self.tokens.get(self.pos - 1).map_or(false, |t| t.kind == TokenKind::Newline) {
                return;
            }

            // Also, certain keywords often start a new statement.
            match self.current_kind() {
                TokenKind::Function
                | TokenKind::Struct
                | TokenKind::Enum
                | TokenKind::If
                | TokenKind::While
                | TokenKind::For
                | TokenKind::Return
                | TokenKind::Import => {
                    return;
                }
                _ => {}
            }
            self.advance();
        }
    }

    /// Expects and consumes an identifier token.
    /// Returns the identifier's string and span, or pushes an error and returns `None`.
    fn expect_identifier(&mut self, errors: &mut Vec<ParserError>) -> Option<(String, Span)> {
        match self.current_kind().clone() {
            TokenKind::Identifier(s) => {
                let span = self.current_span();
                self.advance();
                Some((s, span))
            }
            _ => {
                errors.push(ParserError::ExpectedIdentifier {
                    span: self.current_span(),
                });
                None
            }
        }
    }

    /// Expects and consumes a type identifier token (starts with an uppercase letter).
    /// Returns the type name string and span, or pushes an error and returns `None`.
    fn expect_type_identifier(&mut self, errors: &mut Vec<ParserError>) -> Option<(String, Span)> {
        match self.current_kind().clone() {
            TokenKind::Type(s) => {
                let span = self.current_span();
                self.advance();
                Some((s, span))
            }
            _ => {
                errors.push(ParserError::ExpectedTypeIdentifier {
                    span: self.current_span(),
                });
                None
            }
        }
    }

    /// Expects and consumes an identifier OR a type name token.
    /// Returns the name string and span, or an error. This is useful
    /// for parsing constructs that can accept either casing, like module names.
    fn expect_module_name(&mut self, errors: &mut Vec<ParserError>) -> Option<(String, Span)> {
        match self.current_kind().clone() {
            TokenKind::Identifier(s) | TokenKind::Type(s) => {
                let span = self.current_span();
                self.advance();
                Some((s, span))
            }
            _ => {
                errors.push(ParserError::SyntaxError {
                    message: "Expected a module name (e.g., 'my_module' or 'MyType')".to_string(),
                    span: self.current_span(),
                });
                None
            }
        }
    }

    /// Expects and consumes an identifier OR a type name token.
    /// This is useful for parsing any segment of an import path.
    fn expect_path_segment(&mut self, errors: &mut Vec<ParserError>) -> Option<(String, Span)> {
        match self.current_kind().clone() {
            TokenKind::Identifier(s) | TokenKind::Type(s) => {
                let span = self.current_span();
                self.advance();
                Some((s, span))
            }
            _ => {
                errors.push(ParserError::SyntaxError {
                    message: "Expected an identifier or type name as part of the import path".to_string(),
                    span: self.current_span(),
                });
                None
            }
        }
    }

    /// Consumes any sequence of one or more newline tokens.
    fn skip_newlines(&mut self) {
        while *self.current_kind() == TokenKind::Newline {
            self.advance();
        }
    }

    /// Consumes any `DocComment` tokens and joins their contents.
    fn parse_optional_doc_comment(&mut self, _errors: &mut Vec<ParserError>) -> Option<Option<String>> {
        let mut comments = Vec::new();
        while let TokenKind::DocComment(content) = self.current_kind() {
            comments.push(content.clone());
            self.advance();
            self.skip_newlines();
        }
        if comments.is_empty() {
            Some(None)
        } else {
            Some(Some(comments.join("\n")))
        }
    }

    /// Consumes any `ModuleDocComment` tokens at the beginning of a file.
    fn parse_module_doc_comment(&mut self, _errors: &mut Vec<ParserError>) -> Option<Option<String>> {
        let mut comments = Vec::new();
        while let TokenKind::ModuleDocComment(content) = self.current_kind() {
            comments.push(content.clone());
            self.advance();
            self.skip_newlines();
        }

        if comments.is_empty() {
            Some(None)
        } else {
            Some(Some(comments.join("\n")))
        }
    }

    /// Scans ahead to find the matching `>` for a `<` at a given position.
    /// Returns the index of the `>` if found. This does not consume tokens.
    fn find_matching_rangle(&self, start_pos: usize) -> Option<usize> {
        // We expect the token at start_pos to be LAngle, so we start scanning after it with a balance of 1.
        if self.tokens.get(start_pos)?.kind != TokenKind::LAngle {
            return None;
        }

        let mut balance = 1;
        for i in (start_pos + 1)..self.tokens.len() {
            match self.tokens[i].kind {
                TokenKind::LAngle => balance += 1,
                TokenKind::RAngle => {
                    balance -= 1;
                    if balance == 0 {
                        return Some(i);
                    }
                }
                TokenKind::GreaterGreater => {
                    // Handle the ambiguous case
                    balance -= 2;
                    if balance == 0 {
                        return Some(i);
                    }
                    if balance < 0 {
                        // This implies an unmatched '>>', which is a syntax error, but
                        // for lookahead, we just fail to find a match.
                        return None;
                    }
                }
                TokenKind::Eof => return None, // Reached end without matching
                _ => {}
            }
        }
        None // No matching RAngle found
    }

    /// Consumes a `>` token, handling the `>>` ambiguity by splitting the token if necessary.
    /// This allows parsing of nested generic types like `Vector<Vector<Int64>>`.
    fn eat_closing_rangle(&mut self, errors: &mut Vec<ParserError>) -> Option<Token> {
        match self.current_kind() {
            TokenKind::RAngle => Some(self.advance().clone()),
            TokenKind::GreaterGreater => {
                let current_token = self.advance().clone();
                let span1 = Span::new(current_token.span.start, current_token.span.start + 1);
                let span2 = Span::new(current_token.span.start + 1, current_token.span.end);

                let token1 = Token {
                    kind: TokenKind::RAngle,
                    span: span1,
                };
                let token2 = Token {
                    kind: TokenKind::RAngle,
                    span: span2,
                };

                // Splice the second '>' back into the token stream. The first one is "consumed" and returned.
                self.tokens.insert(self.pos, token2);

                Some(token1)
            }
            _ => {
                errors.push(ParserError::UnexpectedToken {
                    expected: "a closing '>' for the generic parameter list".to_string(),
                    found: format!("{:?}", self.current_kind()),
                    span: self.current_span(),
                });
                None
            }
        }
    }

    /// A generic helper to parse a comma-separated list of items between `<` and `>`.
    /// It uses a provided parsing function for the items and correctly handles the `>>` ambiguity.
    fn parse_generic_list<T, F>(&mut self, mut item_parser: F, errors: &mut Vec<ParserError>) -> Option<Vec<T>>
    where
        F: FnMut(&mut Self, &mut Vec<ParserError>) -> Option<T>,
    {
        self.eat(&TokenKind::LAngle, errors)?;
        let mut items = Vec::new();
        if *self.current_kind() != TokenKind::RAngle && *self.current_kind() != TokenKind::GreaterGreater {
            loop {
                items.push(item_parser(self, errors)?);
                if *self.current_kind() == TokenKind::Comma {
                    self.advance();
                } else {
                    break;
                }
            }
        }
        self.eat_closing_rangle(errors)?;
        Some(items)
    }

    /// Parses a block of statements until one of the specified terminator tokens is found.
    /// This is the new, context-aware block parser.
    ///
    /// # Arguments
    /// * `terminators` - A slice of `TokenKind`s that will end the block.
    /// * `allow_yield` - A boolean indicating if `yield` is a valid statement in this context.
    pub(super) fn parse_block(
        &mut self,
        terminators: &[TokenKind],
        allow_yield: bool,
        errors: &mut Vec<ParserError>,
    ) -> Vec<Statement> {
        let mut statements = Vec::new();
        self.skip_newlines();
        // Loop until we hit one of the terminator tokens.
        while !terminators.iter().any(|t| std::mem::discriminant(t) == std::mem::discriminant(self.current_kind())) && *self.current_kind() != TokenKind::Eof {
            if let Some(stmt) = self.parse_statement(allow_yield, errors) {
                statements.push(stmt);
            } else {
                // An error occurred during statement parsing. Synchronize to continue.
                self.synchronize();
            }
            self.skip_newlines();
        }
        statements
    }

    /// The top-level parsing function that parses the entire program.
    fn parse_program(&mut self, errors: &mut Vec<ParserError>) -> Program {
        self.skip_newlines();
        let module_doc_comment = self.parse_module_doc_comment(errors).flatten();
        self.skip_newlines();

        // The program is a block terminated by End-of-File. Yield is not allowed at the top level.
        let statements = self.parse_block(&[TokenKind::Eof], false, errors);

        ParsedProgram {
            statements,
            module_doc_comment,
        }
    }
}

// Add helper methods to TokenKind for the parser lookahead logic.
impl TokenKind {
    fn is_identifier(&self) -> bool {
        matches!(self, TokenKind::Identifier(_))
    }

    fn is_type(&self) -> bool {
        matches!(self, TokenKind::Type(_))
    }
}