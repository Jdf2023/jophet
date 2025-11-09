// src/diagnostics/mod.rs
//! Handles the formatting and printing of compiler diagnostics and status messages.
//!
//! This module uses the `ariadne` crate to generate beautiful, user-friendly error
//! reports that point directly to the location of an issue in the source code. It
//! also contains a collection of styled printing functions for informational messages
//! like "Compiling", "Finished", etc., using the `colored` crate. It now supports
//! reporting multiple errors from a single compilation pass.

use crate::diagnostics::errors::JophetError;
use ariadne::{Color, Fmt, Label, Report, ReportKind, Source};
use colored::*;
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Defines the various error types used by the compiler.
pub mod errors;

/// Normalizes the path separators in a path string for consistent display.
/// Replaces `/` with `\` on Windows, and `\` with `/` on other platforms.
fn normalize_path_for_display(path: &Path) -> String {
    let path_str = path.to_string_lossy();
    if cfg!(windows) {
        path_str.replace('/', "\\")
    } else {
        path_str.replace('\\', "/")
    }
}

/// Selects a color for the diagnostic report based on its severity.
fn get_color_spec(kind: ReportKind) -> Color {
    match kind {
        ReportKind::Error => Color::Red,
        ReportKind::Warning => Color::Yellow,
        ReportKind::Advice => Color::Cyan,
        _ => Color::Cyan,
    }
}

/// Generates and prints a diagnostic report for a given compiler error.
///
/// It uses a source cache to avoid re-reading files from disk for multiple errors
/// in the same file. It builds a report using `ariadne`, adding the error code,
/// message, and a labeled source span. Path separators are normalized for consistent
/// display across all platforms.
///
/// # Arguments
/// * `source_cache` - A mutable map to cache the contents of source files.
/// * `error` - The `JophetError` to report.
pub fn emit_diagnostic(
    source_cache: &mut HashMap<PathBuf, String>,
    error: &JophetError,
) {
    let (span, path) = error.get_span_and_path();
    let normalized_path_str = normalize_path_for_display(&path);
    let kind = error.get_kind();
    let color = get_color_spec(kind);

    // Read the source file, using the cache if possible.
    let source_code = source_cache.entry(path.clone()).or_insert_with(|| {
        fs::read_to_string(&path).unwrap_or_else(|_| "Could not read source file.".to_string())
    });

    // Build the report piece by piece.
    let mut report = Report::build(kind, &normalized_path_str, span.start)
        .with_code(error.get_code())
        .with_message(error.to_string().fg(color));

    let label_msg = error
        .get_label()
        .unwrap_or_else(|| "here".to_string())
        .fg(color);

    report = report.with_label(
        Label::new((normalized_path_str.clone(), span))
            .with_message(label_msg)
            .with_color(color),
    );

    if let Some(hint) = error.get_hint() {
        report = report.with_help(hint);
    }

    // Print the finalized report to standard error.
    report
        .finish()
        .print((
            normalized_path_str,
            Source::from(source_code),
        ))
        .unwrap();
}

/// The main error handling function for a collection of compiler-specific errors (`JophetError`).
/// It iterates through all errors and emits a formatted diagnostic for each one.
pub fn handle_multiple_errors(errors: &[JophetError]) {
    let mut source_cache = HashMap::new();
    eprintln!();
    for error in errors {
        emit_diagnostic(&mut source_cache, error);
    }
    eprintln!(
        "\nAborting due to {} previous error(s).",
        errors.len().to_string().red().bold()
    );
    eprintln!();
}

/// A generic error handler for non-compiler errors (e.g., I/O errors).
pub fn handle_generic_error(err: Box<dyn Error>) {
    eprintln!();
    eprintln!("{} {}", "error:".red().bold(), err);
    eprintln!();
}

/// Prints a "Compiling" status message.
pub fn print_compiling(pkg_name: &str, version: &str) {
    println!(
        "{} {} v{}",
        "   Compiling".green().bold(),
        pkg_name,
        version
    );
}

/// Prints a "Documenting" status message.
pub fn print_documenting(pkg_name: &str, version: &str) {
    println!(
        "{} {} v{}",
        " Documenting".green().bold(),
        pkg_name,
        version
    );
}

/// Prints a "Finished" status message with the build profile and duration.
pub fn print_finished(profile: &str, duration: Duration) {
    println!(
        "{} {} target(s) in {:.2}s",
        "    Finished".green().bold(),
        profile,
        duration.as_secs_f64()
    );
}

/// Prints a "Finished" status message for the documentation command.
pub fn print_finished_doc(output_path: &Path, duration: Duration) {
    println!(
        "{} documentation in {:.2}s",
        "    Finished".green().bold(),
        duration.as_secs_f64()
    );
    println!(
        " You can find the docs at '{}'",
        normalize_path_for_display(output_path)
    );
}

/// Prints a "Running" status message with the path to the artifact being executed.
pub fn print_running(artifact_path: &Path) {
    println!("{} `{:?}`", "     Running".green().bold(), artifact_path);
}

/// Prints an "Installing" status message.
pub fn print_installing() {
    println!("{}", "  Installing".green().bold());
}

/// Prints an "Installed" status message with the package name and destination.
pub fn print_installed(pkg_name: &str, install_dir: &Path) {
    println!(
        "   {} `{}` to `{:?}`",
        "Installed".green().bold(),
        pkg_name,
        install_dir
    );
}

/// Prints a "Cleaning" status message.
pub fn print_cleaning() {
    println!("    {} `target` directory", "Cleaning".green().bold());
}

/// Prints a "Created" status message for a new package.
pub fn print_creating(pkg_type: &str, name: &str) {
    println!(
        "{} {} (application) `{}` package",
        "     Created".green().bold(),
        pkg_type,
        name
    );
}