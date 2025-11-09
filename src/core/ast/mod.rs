// src/core/ast/mod.rs
//! The Abstract Syntax Tree (AST) module for the Jophet language.
//!
//! This module defines the core data structures that represent the code's structure.
//! It is divided into three main parts:
//!
//! - `common`: Contains fundamental, shared definitions like `Token`, `Span`, and `Literal`
//!   that are used throughout the compiler's frontend.
//! - `untyped`: Defines the AST as it is produced by the parser. In this form, type
//!   annotations are just strings, and no semantic validation has been performed.
//! - `typed`: Defines the AST after it has been processed by the semantic analyzer.
//!   Every expression is annotated with a resolved `JophetType`, and the structure
//!   is guaranteed to be semantically correct. This is the representation that gets
//!   passed to the backend for code generation.
//!
//! The compiler pipeline transforms the code from a stream of tokens into an `untyped`
//! AST and then into a `typed` AST.

/// Contains common definitions shared across different AST representations.
mod common;
/// Defines the semantically-checked and fully-typed AST.
pub mod typed;
/// Defines the initial AST produced by the parser.
pub mod untyped;

// Re-export the most common types for easier access from other modules.
pub use common::{Literal, Span, Token, TokenKind};