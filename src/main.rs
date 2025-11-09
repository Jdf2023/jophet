// src/main.rs
//! The main entry point for the Jophet compiler's command-line interface (CLI).
//!
//! This file is responsible for:
//! 1. Defining the CLI structure, including commands and arguments, using the `clap` crate.
//! 2. Parsing the command-line arguments provided by the user.
//! 3. Dispatching to the appropriate command handler in the `commands` module.
//! 4. Handling top-level errors, distinguishing between compiler-specific `JophetError`s
//!    (which are formatted nicely with precise source locations) and other generic errors.
//!    This now includes support for reporting multiple errors from a single compilation pass.

use crate::backend::BackendType;
use crate::commands::build::MultipleErrors;
use crate::diagnostics::handle_generic_error;
use clap::{
    builder::styling::{AnsiColor, Color, Style, Styles},
    Parser, Subcommand,
};
use diagnostics::errors::JophetError;
use std::collections::HashMap;
use std::error::Error;

// Module declarations for the entire project.
mod backend;
mod commands;
mod config;
mod core;
mod diagnostics;
mod docs;

// Defines the custom color scheme for the help output using clap's official styling API.
fn styles() -> Styles {
    Styles::styled()
        .header(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        )
        .usage(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Yellow))),
        )
        .literal(
            Style::new()
                .bold()
                .fg_color(Some(Color::Ansi(AnsiColor::Green))),
        )
        .placeholder(Style::new().fg_color(Some(Color::Ansi(AnsiColor::Blue))))
}

// The description to be printed at the top of the help message for the main command.
const JOPHET_DESCRIPTION: &str =
    concat!("\x1b[1;36m", "jophet - The official compiler for the Jophet programming language.", "\x1b[0m");

// A custom help template to remove the extra newline after the main description.
const HELP_TEMPLATE: &str = "{before-help}{usage-heading} {usage}\n\n{all-args}";

#[derive(Parser)]
#[command(
    author,
    version,
    long_about = None,
    before_help = JOPHET_DESCRIPTION,
    help_template = HELP_TEMPLATE,
    styles = styles(),
    disable_help_subcommand = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Sets the compiler backend. Overrides the profile setting in Jophet.toml.
    #[arg(long, short = 'b', global = true, value_name = "BACKEND", value_enum, require_equals = true)]
    backend: Option<BackendType>,

    /// Keep intermediate build files. Overrides the profile setting in Jophet.toml.
    #[arg(long, short = 'k', global = true)]
    keep_intermediate: bool,

    /// Statically link the final executable. Overrides the profile setting in Jophet.toml.
    #[arg(long, global = true, name = "static")]
    static_build: bool,

    /// Set the compilation target triple (e.g., x86_64-pc-windows-msvc, armv7-unknown-linux-gnueabihf).
    /// If not provided, defaults to the host machine's architecture.
    #[arg(long, global = true)]
    target: Option<String>,

    /// List all supported compilation targets and exit.
    #[arg(long, global = true)]
    list_targets: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new Jophet package in a new directory
    #[command(long_about = "\x1b[1;36mCreate a new Jophet package in a new directory\x1b[0m")]
    New {
        /// The name of the new package.
        name: String,
        /// Create a binary (application) package.
        #[arg(long, conflicts_with = "lib")]
        bin: bool,
        /// Create a library package.
        #[arg(long)]
        lib: bool,
    },
    /// Compile the current Jophet package
    #[command(long_about = "\x1b[1;36mCompile the current Jophet package\x1b[0m")]
    Build {
        /// Build with release optimizations.
        #[arg(long, short = 'r')]
        release: bool,
    },
    /// Build and run a binary package
    #[command(long_about = "\x1b[1;36mBuild and run a binary package\x1b[0m")]
    Run {
        /// Run with release optimizations.
        #[arg(long, short = 'r')]
        release: bool,
        /// Arguments to pass to the program.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Start an interactive REPL session (default)
    #[command(long_about = "\x1b[1;36mStart an interactive REPL session (this is the default action if no subcommand is given)\x1b[0m")]
    Repl,
    /// Build and install a package to the shared Jophet directory (~/.jophet)
    #[command(long_about = "\x1b[1;36mBuild and install a package to the shared Jophet directory (~/.jophet)\n\nNote: This command always builds the package in release mode.\x1b[0m")]
    Install,
    /// Remove the `target` directory and all build artifacts
    #[command(long_about = "\x1b[1;36mRemove the `target` directory and all build artifacts\x1b[0m")]
    Clean,
    /// Generate HTML documentation for the current package
    #[command(name = "document", long_about = "\x1b[1;36mGenerate HTML documentation for the current package\x1b[0m")]
    Document,
}

/// The primary application logic.
///
/// Takes a fully parsed `Cli` struct and calls the corresponding handler function.
fn run(cli: Cli) -> Result<(), Box<dyn Error>> {
    // First, handle top-level flags that exit immediately.
    if cli.list_targets {
        println!("Supported compilation targets:");
        for target in backend::get_supported_targets() {
            println!("  {}", target);
        }
        return Ok(());
    }
    
    // If a subcommand is provided, run it. Otherwise, default to the REPL.
    if let Some(command) = &cli.command {
        match command {
            Commands::New { name, bin, lib } => {
                // Default to `bin` if neither flag is provided.
                let is_bin = !*lib;
                commands::new::handle_new(name, is_bin)?;
            }
            Commands::Build { release } => {
                commands::build::handle_build(
                    *release,
                    false,
                    cli.keep_intermediate,
                    cli.backend,
                    cli.static_build,
                    cli.target.clone(),
                )?;
            }
            Commands::Run { release, args } => {
                commands::run::handle_run(
                    *release,
                    args.clone(),
                    cli.keep_intermediate,
                    cli.backend,
                    cli.static_build,
                    cli.target.clone(),
                )?;
            }
            Commands::Repl => {
                commands::repl::handle_repl(cli.backend.unwrap_or(BackendType::C))?;
            }
            Commands::Install => {
                commands::install::handle_install()?;
            }
            Commands::Clean => {
                commands::clean::handle_clean()?;
            }
            Commands::Document => {
                commands::document::handle_document(cli.backend.unwrap_or(BackendType::C))?;
            }
        }
    } else {
        // Default behavior: run the REPL.
        commands::repl::handle_repl(cli.backend.unwrap_or(BackendType::C))?;
    }

    Ok(())
}

/// The main entry point of the `jophet` executable.
///
/// It calls `Cli::try_parse` to handle arguments manually. This allows us to
/// intercept requests for help and version information, print newlines for
/// clean formatting, and then let clap render the appropriate message.
fn main() {
    match Cli::try_parse() {
        Ok(cli) => {
            // If parsing is successful, execute the main application logic.
            if let Err(e) = run(cli) {
                // Check if the error is our special container for multiple errors.
                if let Some(multi_error) = e.downcast_ref::<MultipleErrors>() {
                    diagnostics::handle_multiple_errors(&multi_error.0);
                }
                // Check if it's a single, standard JophetError.
                else if let Some(single_error) = e.downcast_ref::<JophetError>() {
                    let mut cache = HashMap::new();
                    diagnostics::emit_diagnostic(&mut cache, single_error);
                    eprintln!();
                }
                // Otherwise, it's a generic error (e.g., IO error).
                else {
                    handle_generic_error(e);
                }
                std::process::exit(1);
            }
        }
        Err(e) => {
            // A parsing error occurred. This is often just a request for --help or --version.
            // We print a newline first to ensure clean spacing from the command prompt.
            println!();

            // Let clap render the error or help/version message.
            e.print().unwrap();

            // Print a final newline for spacing before the next prompt.
            println!();

            // Exit with the appropriate status code provided by clap.
            std::process::exit(e.exit_code());
        }
    }
}