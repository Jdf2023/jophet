// src/commands/run.rs
//! Handles the `jophet run` command.
//!
//! This command is a convenient shortcut that first builds the project and then,
//! if the build is successful and the project is a binary, executes the resulting
//! artifact. Any arguments passed after the command are forwarded to the running program.

use crate::commands::build;
use crate::diagnostics::{self, errors::JophetError};
use crate::backend::BackendType;
use std::error::Error;
use std::process::Command;

/// The main entry point for the `run` command.
///
/// It first calls the `handle_build` function, passing along any relevant flags.
/// If the build succeeds, it checks if the artifact is an executable. If so, it
/// spawns a new process to run it, passing along any additional arguments.
///
/// # Arguments
/// * `is_release` - If `true`, builds and runs the release version. Otherwise, the debug version.
/// * `args` - A vector of strings representing the arguments to be passed to the executed program.
/// * `keep_intermediate` - Passed to `handle_build` to control cleanup of build files.
/// * `backend_type` - Passed to `handle_build` to select the compiler backend.
/// * `static_build` - Passed to `handle_build` to create a statically linked executable.
/// * `target` - An optional string specifying the target triple for cross-compilation.
pub fn handle_run(
    is_release: bool,
    args: Vec<String>,
    keep_intermediate: bool,
    backend_type: Option<BackendType>,
    static_build: bool,
    target: Option<String>,
) -> Result<(), Box<dyn Error>> {
    // First, build the project.
    let artifact = build::handle_build(is_release, false, keep_intermediate, backend_type, static_build, target)?;

    // A library package cannot be run.
    if artifact.is_lib {
        return Err(Box::new(JophetError::CannotRunLibrary));
    }

    diagnostics::print_running(&artifact.path);

    // Execute the compiled binary.
    let status = Command::new(&artifact.path).args(args).status()?;

    if !status.success() {
        return Err(Box::new(JophetError::ExecutionFailed {
            status: status.to_string(),
        }));
    }

    Ok(())
}