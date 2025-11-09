// src/commands/clean.rs
//! Handles the `jophet clean` command.
//!
//! This is a simple utility command that removes the `target` directory,
//! deleting all previous build artifacts and intermediate files.

use crate::diagnostics;
use std::error::Error;
use std::fs;
use std::path::Path;

/// The main entry point for the `clean` command.
///
/// It checks for the existence of the `target` directory in the current
/// project and removes it if found.
pub fn handle_clean() -> Result<(), Box<dyn Error>> {
    let target_dir = Path::new("target");
    if target_dir.exists() {
        fs::remove_dir_all(target_dir)?;
        diagnostics::print_cleaning();
    } else {
        println!("`target` directory not found, nothing to clean.");
    }
    Ok(())
}