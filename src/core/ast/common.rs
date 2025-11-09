// src/core/ast/common.rs
//! Contains common, shared definitions used across the Abstract Syntax Tree (AST).
//!
//! This module defines fundamental building blocks like `Span`, `Token`, `TokenKind`,
//! and `Literal`, which are used by the lexer, parser, and semantic analyzer.

use std::ops::Range;

/// Represents a region of the source code.
///
/// A `Span` marks the start and end byte position of a token or an AST node,
/// allowing for precise error reporting.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span {
    /// The starting byte index of the span (inclusive).
    pub start: usize,
    /// The ending byte index of the span (exclusive).
    pub end: usize,
}

impl From<Range<usize>> for Span {
    fn from(range: Range<usize>) -> Self {
        Span {
            start: range.start,
            end: range.end,
        }
    }
}

impl Span {
    /// Creates a new `Span`.
    pub fn new(start: usize, end: usize) -> Self {
        Span { start, end }
    }

    /// Merges two spans to create a new span that encompasses both.
    /// The new span starts at the minimum of the two start positions and ends
    /// at the maximum of the two end positions.
    pub fn merge(&self, other: &Self) -> Self {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Converts the span into a `Range<usize>`.
    pub fn to_range(&self) -> Range<usize> {
        self.start..self.end
    }
}

/// Represents a single lexical token from the source code.
/// It contains a `TokenKind` and its `Span`.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The type of the token.
    pub kind: TokenKind,
    /// The location of the token in the source code.
    pub span: Span,
}

/// An enum representing all possible kinds of tokens in the Jophet language.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Delete,
    New,
    Public,
    Import,
    Function,
    End,
    Return,
    /// A qualifier that marks a declaration for compile-time evaluation.
    ///
    /// When used before a variable declaration, `const` requires the initializer
    /// to be a compile-time constant expression. The value is evaluated during
    /// compilation and embedded directly in the output. This does not change
    /// other language semantics; it is distinct from `mutable` which controls
    /// mutability at runtime.
    Const,
    Mutable,
    If,
    Else,
    While,
    For,
    In,
    Struct,
    Implement,
    Enum,
    Union,
    TaggedUnion,
    Error,
    Try,
    Catch,
    Switch,
    Case,
    Of,
    Do,
    Yield,
    Break,
    Continue,
    Allow,
    Trait,
    Raw,
    
    // Doc comments and module docs

    /// A documentation comment, e.g., `/// My function`.
    DocComment(String),
    /// A module-level documentation comment, e.g., `//! This module`.
    ModuleDocComment(String),

    // Identifiers and Types
    /// A variable or function name, starting with a lowercase letter.
    Identifier(String),
    /// A type name, starting with an uppercase letter.
    Type(String),

    // Literals
    IntLiteral(i64),
    FloatLiteral(f64),
    StringLiteral(String),
    CharLiteral(char),
    BoolLiteral(bool),
    NothingLiteral,

    // Operators and Punctuation
    Plus,
    PlusEquals,
    Minus,
    MinusEquals,
    Asterisk,
    AsteriskEquals,
    AsteriskAsterisk,
    AsteriskAsteriskEquals,
    Slash,
    SlashEquals,
    Percent,
    PercentEquals,
    Equal,
    EqualEqual,
    FatArrow, // =>
    Bang,
    BangEquals,
    LessEquals,
    GreaterEquals,
    Ampersand,
    AmpersandAmpersand,
    AmpersandEquals,
    Pipe,
    PipePipe,
    PipeEquals,
    Caret,
    CaretEquals,
    Tilde,
    LessLess,
    LessLessEquals,
    GreaterGreater,
    GreaterGreaterEquals,
    Question,
    Colon,
    DoubleColon,
    Comma,
    LParen,
    RParen,
    LBracket,
    RBracket,
    Dot,
    DoubleDot,
    LAngle,
    RAngle,

    // Whitespace and Control
    Newline,
    /// End of File marker.
    Eof,
}

/// An enum representing a literal value in the source code.
/// This is used in the AST to store the value of literals.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    String(String),
    Char(char),
    Bool(bool),
    Nothing,
}