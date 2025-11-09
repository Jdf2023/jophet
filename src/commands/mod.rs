// src/commands/mod.rs
//! The main module for CLI command handlers.
//!
//! This module acts as a parent for all the sub-modules that implement the logic
//! for the various `jophet` subcommands (e.g., `build`, `run`, `new`). It re-exports
//! them for easy access from the main application entry point.

/// Implements the `jophet build` command logic.
pub mod build;
/// Implements the `jophet clean` command logic.
pub mod clean;
/// Implements the `jophet doc` command logic.
pub mod document;
/// Implements the `jophet install` command logic.
pub mod install;
/// Implements the `jophet new` command logic.
pub mod new;
/// Implements the interactive REPL, the default action and `jophet repl` command.
pub mod repl;
/// Implements the `jophet run` command logic.
pub mod run;