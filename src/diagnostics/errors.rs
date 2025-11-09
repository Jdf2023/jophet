// src/diagnostics/errors.rs
//! Defines the error types used throughout the Jophet compiler.
//!
//! This module centralizes all error definitions, providing a structured way to
//! represent issues that can occur during any compilation stage, from lexing to
//! linking. The main error enum is `JophetError`, which categorizes errors into
//! `LexerError`, `ParserError`, `SemanticError`, and general build failures.
//!
//! These error types are designed to integrate with the `ariadne` crate to produce
//! user-friendly, formatted diagnostic messages.

use crate::core::ast::Span;
use ariadne::ReportKind;
use std::error::Error;
use std::io;
use std::ops::Range;
use std::path::PathBuf;

/// The top-level error enum for the entire compilation process.
#[derive(Debug)]
pub enum JophetError {
    /// An error occurred during the final build/linking stage (e.g., C compiler error).
    BuildFailed {
        reason: String,
    },
    /// An attempt was made to run a library package, which is not executable.
    CannotRunLibrary,
    /// The user's program was compiled successfully but exited with a non-zero status code.
    ExecutionFailed {
        status: String,
    },
    /// An error that occurred during lexical analysis.
    LexerError {
        error: LexerError,
        file_path: PathBuf,
    },
    /// An error that occurred during parsing (syntax error).
    ParserError {
        error: ParserError,
        file_path: PathBuf,
    },
    /// An error that occurred during semantic analysis (type error, name error, etc.).
    SemanticError(SemanticError),
}

/// Errors that can occur during the lexing phase.
#[derive(Debug)]
pub enum LexerError {
    UnexpectedCharacter { char: char, span: Span },
    UnterminatedString { span: Span },
    UnterminatedChar { span: Span },
    UnterminatedMultiLineComment { span: Span },
    UnexpectedEof,
    InvalidEscapeSequence { char: char, span: Span },
    InvalidUnicodeEscape { message: String, span: Span },
}

/// Errors that can occur during the parsing phase.
#[derive(Debug)]
pub enum ParserError {
    UnexpectedToken {
        expected: String,
        found: String,
        span: Span,
    },
    ExpectedIdentifier {
        span: Span,
    },
    ExpectedTypeIdentifier {
        span: Span,
    },
    ExpectedVariantName {
        span: Span,
    },
    ExpectedFieldOrTupleIndex {
        span: Span,
    },
    InvalidCallTarget {
        span: Span,
    },
    InvalidTypeExpression {
        span: Span,
    },
    UnmatchedBraceInFormatString {
        span: Span,
    },
    InvalidExpressionInFormatString {
        span: Span,
    },
    /// A general syntax error, often used for more complex structural violations.
    SyntaxError {
        message: String,
        span: Span,
    },
}

/// Errors that can occur during the semantic analysis phase.
#[derive(Debug, Clone)]
pub enum SemanticError {
    TypeError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    NameError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    MemoryError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    FlowError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    ModuleError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    SyntaxError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    /// An error that occurs during compile-time function execution.
    CtfeError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
    InternalError {
        message: String,
        span: Span,
        file_path: PathBuf,
    },
}

// From implementations to allow for easy conversion between error types using `?`.
impl From<SemanticError> for JophetError {
    fn from(e: SemanticError) -> Self {
        JophetError::SemanticError(e)
    }
}

// Allow conversion from a generic build error into a JophetError for `?` propagation.
impl From<Box<dyn Error>> for JophetError {
    fn from(e: Box<dyn Error>) -> Self {
        JophetError::BuildFailed {
            reason: e.to_string(),
        }
    }
}

// Allow conversion from a standard I/O error into a JophetError.
impl From<io::Error> for JophetError {
    fn from(e: io::Error) -> Self {
        JophetError::BuildFailed {
            reason: format!("I/O Error: {}", e),
        }
    }
}

// Custom conversion from a ParserError (and its context) to a SemanticError.
// This is used when the semantic analyzer re-invokes the parser, for example
// to parse expressions inside an interpolated string.
impl From<(ParserError, PathBuf)> for SemanticError {
    fn from((err, file_path): (ParserError, PathBuf)) -> Self {
        SemanticError::FlowError {
            message: format!("Failed to parse interpolated expression: {}", err),
            span: err.get_span(),
            file_path,
        }
    }
}

// Standard `Display` and `Error` trait implementations.
impl std::fmt::Display for JophetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JophetError::BuildFailed { reason } => write!(f, "Build failed: {}", reason),
            JophetError::CannotRunLibrary => write!(f, "Cannot execute a library package"),
            JophetError::ExecutionFailed { status } => {
                write!(f, "Process finished with non-zero exit code: {}", status)
            }
            JophetError::LexerError { error, .. } => write!(f, "Lexical Error: {}", error),
            JophetError::ParserError { error, .. } => write!(f, "Syntax Error: {}", error),
            JophetError::SemanticError(e) => write!(f, "{}", e),
        }
    }
}

impl std::error::Error for JophetError {}

impl std::fmt::Display for LexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LexerError::UnexpectedCharacter { char, .. } => {
                write!(f, "Unexpected character '{}'", char)
            }
            LexerError::UnterminatedString { .. } => write!(f, "Unterminated string literal"),
            LexerError::UnterminatedChar { .. } => write!(f, "Unterminated character literal"),
            LexerError::UnterminatedMultiLineComment { .. } => {
                write!(f, "Unterminated multi-line comment")
            }
            LexerError::UnexpectedEof => write!(f, "Unexpected end of file"),
            LexerError::InvalidEscapeSequence { char, .. } => {
                write!(f, "Invalid escape sequence: '\\{}'", char)
            }
            LexerError::InvalidUnicodeEscape { message, .. } => {
                write!(f, "Invalid unicode escape: {}", message)
            }
        }
    }
}

impl std::fmt::Display for ParserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParserError::UnexpectedToken {
                expected, found, ..
            } => write!(f, "Unexpected token. Expected {}, found {}", expected, found),
            ParserError::ExpectedIdentifier { .. } => write!(f, "Expected an identifier"),
            ParserError::ExpectedTypeIdentifier { .. } => {
                write!(
                    f,
                    "Expected a type name (starting with an uppercase letter)"
                )
            }
            ParserError::ExpectedVariantName { .. } => write!(f, "Expected a variant name"),
            ParserError::ExpectedFieldOrTupleIndex { .. } => {
                write!(f, "Expected a field name or integer tuple index")
            }
            ParserError::InvalidCallTarget { .. } => {
                write!(f, "This expression cannot be called as a function")
            }
            ParserError::InvalidTypeExpression { .. } => {
                write!(
                    f,
                    "A type name cannot be used as a standalone expression here"
                )
            }
            ParserError::UnmatchedBraceInFormatString { .. } => {
                write!(f, "Unmatched opening brace `{{` in format string")
            }
            ParserError::InvalidExpressionInFormatString { .. } => {
                write!(
                    f,
                    "Could not parse the expression inside format string braces `{{...}}`"
                )
            }
            ParserError::SyntaxError { message, .. } => write!(f, "Syntax Error: {}", message),
        }
    }
}

impl std::fmt::Display for SemanticError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SemanticError::TypeError { message, .. } => write!(f, "Type Error: {}", message),
            SemanticError::NameError { message, .. } => write!(f, "Name Error: {}", message),
            SemanticError::MemoryError { message, .. } => write!(f, "Memory Error: {}", message),
            SemanticError::FlowError { message, .. } => {
                write!(f, "Control Flow Error: {}", message)
            }
            SemanticError::ModuleError { message, .. } => write!(f, "Module Error: {}", message),
            SemanticError::SyntaxError { message, .. } => write!(f, "Syntax Error: {}", message),
            SemanticError::CtfeError { message, .. } => {
                write!(f, "Compile-Time Execution Error: {}", message)
            }
            SemanticError::InternalError { message, .. } => {
                write!(f, "Internal Compiler Error: {}", message)
            }
        }
    }
}

impl JophetError {
    /// Gets the `ariadne` `ReportKind` for the error.
    pub fn get_kind(&self) -> ReportKind {
        match self {
            JophetError::BuildFailed { .. }
            | JophetError::CannotRunLibrary
            | JophetError::ExecutionFailed { .. }
            | JophetError::LexerError { .. }
            | JophetError::ParserError { .. }
            | JophetError::SemanticError(_) => ReportKind::Error,
        }
    }

    /// Gets the unique error code (e.g., "E0010") for the error.
    pub fn get_code(&self) -> &'static str {
        match self {
            JophetError::LexerError { .. } => "E0001",
            JophetError::ParserError { .. } => "E0002",
            JophetError::SemanticError(e) => match e {
                SemanticError::TypeError { .. } => "E0010",
                SemanticError::NameError { .. } => "E0011",
                SemanticError::MemoryError { .. } => "E0012",
                SemanticError::FlowError { .. } => "E0013",
                SemanticError::ModuleError { .. } => "E0014",
                SemanticError::SyntaxError { .. } => "E0015",
                SemanticError::CtfeError { .. } => "E0016",
                SemanticError::InternalError { .. } => "E9999",
            },
            _ => "E0000",
        }
    }

    /// Gets the source span and file path of the error, if available.
    pub fn get_span_and_path(&self) -> (Range<usize>, PathBuf) {
        match self {
            JophetError::LexerError { error, file_path } => (error.get_span().to_range(), file_path.clone()),
            JophetError::ParserError { error, file_path } => (error.get_span().to_range(), file_path.clone()),
            JophetError::SemanticError(e) => e.get_span_and_path(),
            _ => (0..0, PathBuf::new()), // Build errors don't have a source span.
        }
    }

    /// Gets the message to be displayed in the primary label of the diagnostic report.
    pub fn get_label(&self) -> Option<String> {
        match self {
            JophetError::LexerError { error, .. } => Some(error.to_string()),
            JophetError::ParserError { error, .. } => Some(error.to_string()),
            JophetError::SemanticError(e) => Some(match e {
                SemanticError::TypeError { message, .. } => message.clone(),
                SemanticError::NameError { message, .. } => message.clone(),
                SemanticError::MemoryError { message, .. } => message.clone(),
                SemanticError::FlowError { message, .. } => message.clone(),
                SemanticError::ModuleError { message, .. } => message.clone(),
                SemanticError::SyntaxError { message, .. } => message.clone(),
                SemanticError::CtfeError { message, .. } => message.clone(),
                SemanticError::InternalError { message, .. } => message.clone(),
            }),
            _ => None,
        }
    }

    /// Gets an optional hint or help message to be displayed with the diagnostic.
    pub fn get_hint(&self) -> Option<String> {
        match self {
            JophetError::SemanticError(SemanticError::MemoryError { message, .. })
                if message.contains("Memory leak detected") =>
            {
                Some("Every variable initialized with `new` must have its ownership handled properly. Usually this means scheduling its deletion with `defer delete <variable>` in the same scope.".into())
            }
             JophetError::SemanticError(SemanticError::InternalError { .. }) => {
                Some("This is a bug in the Jophet compiler. Please report it.".into())
            }
            _ => None,
        }
    }
}

// Helper implementations for getting the `Span` from lower-level error types.
impl LexerError {
    pub fn get_span(&self) -> Span {
        match self {
            LexerError::UnexpectedCharacter { span, .. } => span.clone(),
            LexerError::UnterminatedString { span } => span.clone(),
            LexerError::UnterminatedChar { span } => span.clone(),
            LexerError::UnterminatedMultiLineComment { span } => span.clone(),
            LexerError::UnexpectedEof => Span::default(),
            LexerError::InvalidEscapeSequence { span, .. } => span.clone(),
            LexerError::InvalidUnicodeEscape { span, .. } => span.clone(),
        }
    }
}

impl ParserError {
    pub fn get_span(&self) -> Span {
        match self {
            ParserError::UnexpectedToken { span, .. } => span.clone(),
            ParserError::ExpectedIdentifier { span } => span.clone(),
            ParserError::ExpectedTypeIdentifier { span } => span.clone(),
            ParserError::ExpectedVariantName { span } => span.clone(),
            ParserError::ExpectedFieldOrTupleIndex { span } => span.clone(),
            ParserError::InvalidCallTarget { span } => span.clone(),
            ParserError::InvalidTypeExpression { span } => span.clone(),
            ParserError::UnmatchedBraceInFormatString { span } => span.clone(),
            ParserError::InvalidExpressionInFormatString { span } => span.clone(),
            ParserError::SyntaxError { span, .. } => span.clone(),
        }
    }
}

impl SemanticError {
    fn get_span_and_path(&self) -> (Range<usize>, PathBuf) {
        match self {
            SemanticError::TypeError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::NameError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::MemoryError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::FlowError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::ModuleError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::SyntaxError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::CtfeError { span, file_path, .. } => (span.to_range(), file_path.clone()),
            SemanticError::InternalError { span, file_path, .. } => {
                (span.to_range(), file_path.clone())
            }
        }
    }
}