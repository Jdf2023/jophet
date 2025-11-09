// src/commands/repl.rs
//! Handles the default `jophet` execution and the explicit `jophet repl` command.
//!
//! This module orchestrates an interactive Read-Eval-Print Loop. It uses a robust
//! "stateful library" architecture. Declarations that modify the session's state
//! (functions, variables, structs, imports) are incrementally compiled into a
//! temporary static library. Expressions and statements meant for immediate execution
//! are compiled into a separate, short-lived executable that links against this
//! session library. This provides a truly stateful, performant, and correct
//! interactive experience. It now handles multiple errors without exiting the session.

use crate::backend::{self, BackendType, CompileOptions, OutputFileType};
use crate::commands::build::MultipleErrors;
use crate::core;
use crate::core::ast::untyped;
use crate::diagnostics;
use colored::*;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use std::collections::HashMap;
use std::env;
use std::error::Error;
use std::fs;
use std::io::{stderr, stdout, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

/// State for the REPL session.
struct ReplState {
    /// A string containing all previously entered valid declaration lines of code.
    session_lib_code: String,
    /// The path to the source file for the REPL's stateful library.
    session_lib_path: PathBuf,
    /// The path to the compiled static library artifact for the session.
    session_lib_artifact_path: PathBuf,
    /// The path to the temporary directory for this REPL session.
    temp_dir: PathBuf,
}

impl ReplState {
    /// Creates a new REPL session, including a temporary directory.
    fn new() -> Result<Self, Box<dyn Error>> {
        let temp_dir = env::temp_dir().join(format!("jophet_repl_{}", std::process::id()));
        if temp_dir.exists() {
            fs::remove_dir_all(&temp_dir)?;
        }
        fs::create_dir_all(&temp_dir)?;

        let session_lib_path = temp_dir.join("repl_lib.jophet");
        fs::write(&session_lib_path, "")?;

        let lib_name = if cfg!(windows) {
            "repl_lib.lib"
        } else {
            "librepl_lib.a"
        };
        let session_lib_artifact_path = temp_dir.join(lib_name);

        Ok(ReplState {
            session_lib_code: String::new(),
            session_lib_path,
            session_lib_artifact_path,
            temp_dir,
        })
    }
}

impl Drop for ReplState {
    /// Cleans up the temporary directory when the REPL session ends.
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.temp_dir);
    }
}

const BLOCK_START_KEYWORDS: &[&str] = &[
    "function", "struct", "enum", "union", "tagged", "error", "trait", "implement", "if", "while", "for", "switch",
];

/// The main entry point for the REPL.
pub fn handle_repl(backend_type: BackendType) -> Result<(), Box<dyn Error>> {
    println!("Welcome to the Jophet REPL! Version {}", env!("CARGO_PKG_VERSION"));
    println!("Type ':exit' or press Ctrl-D to exit.");

    let mut rl = DefaultEditor::new()?;
    
    let history_path = home::home_dir().map(|path| path.join(".jophet_history"));

    if let Some(ref path) = history_path {
        let _ = rl.load_history(path);
    } else {
        println!("Warning: Could not find home directory. REPL history will not be saved.");
    }
    
    let mut state = ReplState::new()?;
    let main_prompt = "jophet> ".to_string();
    let continue_prompt = ".. > ".to_string();

    let mut multiline_buffer = String::new();
    let mut nesting_level: u32 = 0;

    loop {
        let prompt = if nesting_level > 0 { &continue_prompt } else { &main_prompt };
        let readline = rl.readline(prompt);

        match readline {
            Ok(line) => {
                let line_for_history = if nesting_level > 0 {
                    format!("    {}", line)
                } else {
                    line.clone()
                };
                let _ = rl.add_history_entry(&line_for_history);

                if line.trim() == ":exit" {
                    break;
                }

                let trimmed_line = line.trim();
                let starts_with_keyword = BLOCK_START_KEYWORDS.iter().any(|kw| trimmed_line.starts_with(kw));

                if nesting_level > 0 || (starts_with_keyword && trimmed_line != "end") {
                    if nesting_level > 0 {
                        multiline_buffer.push_str("    ");
                    }
                    multiline_buffer.push_str(&line);
                    multiline_buffer.push('\n');
                } else {
                    process_line(&line, &mut state, backend_type)?;
                    continue;
                }

                if starts_with_keyword && trimmed_line != "end" {
                    nesting_level += 1;
                }
                if trimmed_line == "end" {
                    if nesting_level > 0 {
                        nesting_level -= 1;
                    }
                }

                if nesting_level == 0 {
                    process_line(&multiline_buffer, &mut state, backend_type)?;
                    multiline_buffer.clear();
                }
            }
            Err(ReadlineError::Interrupted) => {
                if nesting_level > 0 {
                    println!("\n^C (Cancelled multi-line input)");
                    multiline_buffer.clear();
                    nesting_level = 0;
                } else {
                    println!("^C");
                }
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("Ctrl-D");
                break;
            }
            Err(err) => {
                println!("Error: {:?}", err);
                break;
            }
        }
    }

    if let Some(ref path) = history_path {
        let _ = rl.save_history(path);
    }
    
    Ok(())
}

/// Determines if a line of input is a pure expression that should be printed.
fn is_printable_expression(line: &str, temp_file_path: &Path) -> bool {
    let tokens = match core::lexer::tokenize(line, temp_file_path.to_path_buf()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    if let Ok(stmt) = core::parser::parse_single_statement(tokens, temp_file_path.to_path_buf()) {
        matches!(stmt.kind, untyped::StatementKind::ExpressionStatement(_))
    } else {
        false
    }
}

/// Determines if a line of input is a declaration that should be saved to the REPL's state.
fn is_declaration(line: &str, temp_file_path: &Path) -> bool {
    // Simple heuristic: if it starts with a keyword that defines something,
    // or looks like a variable declaration, it's a declaration.
    let trimmed = line.trim();
    if trimmed.starts_with("function")
        || trimmed.starts_with("struct")
        || trimmed.starts_with("enum")
        || trimmed.starts_with("union")
        || trimmed.starts_with("tagged")
        || trimmed.starts_with("error")
        || trimmed.starts_with("import")
    {
        return true;
    }

    // Use the parser for variable declarations
    let tokens = match core::lexer::tokenize(line, temp_file_path.to_path_buf()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    if let Ok(stmt) = core::parser::parse_single_statement(tokens, temp_file_path.to_path_buf()) {
        matches!(stmt.kind, untyped::StatementKind::VariableDecl(_))
    } else {
        false
    }
}


/// Compiles and executes a single line or block of input against the persistent REPL state.
/// This function now implements a stateful library architecture for improved performance and correctness.
fn process_line(
    line: &str,
    state: &mut ReplState,
    backend_type: BackendType,
) -> Result<(), Box<dyn Error>> {
    if line.trim().is_empty() {
        return Ok(());
    }

    let temp_source_path = state.temp_dir.join("exec.jophet"); // Path for executable source

    // The REPL always runs on the host machine, so we use the host triple.
    let host_triple = env!("JOPHET_HOST_TRIPLE").to_string();
    let target_info = backend::resolve_target_info(&host_triple)?;

    if is_declaration(line, &temp_source_path) {
        // --- DECLARATION MODE ---
        // Append the declaration to our library source and re-build the library.
        let new_lib_code = format!("{}\n{}", state.session_lib_code, line);
        fs::write(&state.session_lib_path, &new_lib_code)?;

        // Analyze and build the library. `build_package` is used for this.
        match crate::commands::build::build_package(
            &state.session_lib_path,
            false, // is_release
            false, // is_installing
            false, // keep_intermediate
            backend_type,
            &state.temp_dir, // shared_deps_dir is the temp_dir itself
            &state.temp_dir, // project_root is also the temp_dir
            true,  // is_lib
            true,  // is_repl_mode
            false, // static_build
            target_info,
        ) {
            Ok(_) => {
                // Success! Commit the new code to the session state.
                state.session_lib_code = new_lib_code;
                // println!("Defined.");
            }
            Err(e) => {
                // Build failed. Revert the source file and report error.
                fs::write(&state.session_lib_path, &state.session_lib_code)?;
                if let Some(multi_error) = e.downcast_ref::<MultipleErrors>() {
                    diagnostics::handle_multiple_errors(&multi_error.0);
                } else {
                    diagnostics::handle_generic_error(e);
                }
            }
        }
    } else {
        // --- EXECUTION MODE ---
        // This is an expression or statement to execute now.
        let trimmed_line = line.trim();
        let is_expr = is_printable_expression(line, &temp_source_path);
        let is_already_print_call = trimmed_line.starts_with("print(") || trimmed_line.starts_with("println(");

        let exec_code = if is_expr && !is_already_print_call {
            format!("println({})", line)
        } else {
            line.to_string()
        };

        // The executable needs to import the session library to access its state.
        let final_source = format!("import repl_lib\n{}", exec_code);
        fs::write(&temp_source_path, &final_source)?;

        // Build the temporary executable, linking against our session library.
        match crate::commands::build::build_package(
            &temp_source_path,
            false, // is_release
            false, // is_installing
            false, // keep_intermediate
            backend_type,
            &state.temp_dir, // shared_deps_dir
            &state.temp_dir, // project_root
            false, // is_lib
            true,  // is_repl_mode
            false, // static_build
            target_info,
        ) {
            Ok(artifact) => {
                // Run the executable, letting it inherit stdout/stderr.
                let status = Command::new(&artifact.path).status()?;
                if !status.success() {
                    eprintln!("\n[REPL: Last command exited with an error: {}]", status);
                }
                // Clean up the temporary executable.
                let _ = fs::remove_file(&artifact.path);
            }
            Err(e) => {
                if let Some(multi_error) = e.downcast_ref::<MultipleErrors>() {
                    diagnostics::handle_multiple_errors(&multi_error.0);
                } else {
                    diagnostics::handle_generic_error(e);
                }
            }
        }
    }

    Ok(())
}