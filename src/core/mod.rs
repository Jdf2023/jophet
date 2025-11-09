// src/core/mod.rs
//! The core frontend of the Jophet compiler.
//!
//! This module encapsulates the entire frontend pipeline, which is responsible for
//! taking raw source code and transforming it into a semantically validated,
//! typed Abstract Syntax Tree (`TypedProgram`). It serves as the bridge between the
//! CLI/build system and the compiler's internal stages.
//!
//! The process flows as follows:
//! 1. `lexer`: Converts source text into a stream of tokens.
//! 2. `parser`: Converts the token stream into an untyped AST, collecting all syntax errors.
//! 3. `semantic_analyzer`: Type-checks, resolves symbols, and enforces semantic rules
//!    on the untyped AST, producing the final typed AST and collecting all semantic errors.

use crate::backend::{BackendType, TargetInfo};
use crate::diagnostics::errors::{JophetError, ParserError, SemanticError};
use std::path::{Path, PathBuf};

/// Contains all Abstract Syntax Tree definitions (`untyped` and `typed`).
pub mod ast;
/// The compile-time function execution engine (interpreter).
pub mod ctfe;
/// The lexical analyzer (tokenizer).
pub mod lexer;
/// The syntax analyzer (parser).
pub mod parser;
/// The semantic analyzer (type checker, borrow checker, etc.).
pub mod semantic_analyzer;

// Re-export key types for convenient access by parent modules.
pub use ast::typed::TypedProgram;
pub use semantic_analyzer::AnalysisResult;

/// Runs the entire compiler frontend pipeline on a given source string.
///
/// This function orchestrates the sequence of lexing, parsing, and semantic analysis.
/// It now collects errors from the parser and semantic analyzer and returns them
/// all at once if either phase fails.
///
/// # Arguments
/// * `source` - The raw source code to be compiled.
/// * `project_root` - The root directory of the project, used for resolving modules.
/// * `main_module_path` - The path to the entry point file (`main.jophet` or `lib.jophet`).
/// * `shared_deps_dir` - The directory where dependency artifacts are stored.
/// * `is_release` - Flag indicating if this is a release build.
/// * `keep_intermediate` - Flag to keep temporary build files.
/// * `backend_type` - The backend to use for code generation.
/// * `is_repl_mode` - A special flag for the REPL to treat imported symbols as global.
/// * `target_info` - Information about the target architecture for compilation.
///
/// # Returns
/// An `AnalysisResult` on success, or a `Vec<JophetError>` if any stage of the
/// frontend fails.
pub fn run_frontend(
    source: &str,
    project_root: PathBuf,
    main_module_path: PathBuf,
    shared_deps_dir: &Path,
    is_release: bool,
    keep_intermediate: bool,
    backend_type: BackendType,
    is_repl_mode: bool,
    target_info: &TargetInfo,
) -> Result<AnalysisResult, Vec<JophetError>> {
    // 1. Lexing (remains fail-fast, as parsing is impossible otherwise)
    let tokens = lexer::tokenize(source, main_module_path.clone()).map_err(|e| vec![e])?;
    // 2. Parsing
    let (parsed_program, parse_errors) =
        parser::parse(tokens, main_module_path.clone()).map_err(|e| vec![e])?; // Should not happen if parser returns Vec

    if !parse_errors.is_empty() {
        let jophet_errors = parse_errors
            .into_iter()
            .map(|error| JophetError::ParserError {
                error,
                file_path: main_module_path.clone(),
            })
            .collect();
        return Err(jophet_errors);
    }

    // 3. Semantic Analysis
    let (analysis_result, semantic_errors) = semantic_analyzer::analyze(
        &parsed_program,
        project_root,
        main_module_path,
        shared_deps_dir,
        is_release,
        keep_intermediate,
        backend_type,
        is_repl_mode,
        target_info,
    )
    .map_err(|e| vec![e])?; // For catastrophic internal analyzer errors

    if !semantic_errors.is_empty() {
        let jophet_errors = semantic_errors.into_iter().map(JophetError::from).collect();
        return Err(jophet_errors);
    }

    Ok(analysis_result)
}