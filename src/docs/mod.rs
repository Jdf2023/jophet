// src/docs/mod.rs
//! The documentation generator for the Jophet language.
//!
//! This module contains the logic for transforming the compiler's semantic
//! analysis output into human-readable documentation, currently targeting HTML.
//!
//! Language note: `const` keyword
//! - `const` can prefix a variable declaration to require a compile-time constant initializer.
//! - Example: `const answer: Int64 = 42`
//! - This behaves like Zig's `comptime` for values and does not alter other Jophet semantics.

/// Implements the HTML documentation generator.
pub mod html_generator;