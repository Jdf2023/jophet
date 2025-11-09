// src/commands/new.rs
//! Handles the `jophet new` command.
//!
//! This command scaffolds a new Jophet project directory with a `Jophet.toml`
//! configuration file, a `src` directory, and a sample source file (`main.jophet` for
//! binaries or `lib.jophet` for libraries). It ensures that a complete and valid
//! package is created, whether it's a standalone project or a library within an
//! existing project (for workspace-like structures).

use crate::config;
use crate::diagnostics;
use std::error::Error;
use std::fs;
use std::path::Path;

/// A minimal "Hello, world!" program for new binary projects.
const HELLO_WORLD_BIN: &str = r#"
println("Hello, world!")
"#;

/// A simple example function for new library projects.
const EXAMPLE_LIB: &str = r#"
public function add(x: Int64, y: Int64): Int64
    return x + y
end
"#;

/// Creates a new Jophet package at the specified root path.
///
/// This helper function encapsulates the logic for creating the directory structure,
/// writing the `Jophet.toml` manifest, and creating the initial source file.
fn create_package(root: &Path, name: &str, is_bin: bool) -> Result<(), Box<dyn Error>> {
    if root.exists() {
        return Err(format!("Destination '{}' already exists.", root.display()).into());
    }

    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir)?;

    let project_type = if is_bin { "bin" } else { "lib" };
    let toml_content = format!(
        r#"[package]
name = "{}"
version = "0.1.0"
type = "{}"

# See more keys and their definitions at [TODO: Add link to documentation]

[dependencies]

# Build profiles control compiler settings.

[profile.dev]
# Settings for `jophet build` or `jophet run`
opt-level = 0          # Optimization level (0-3)
debug = true             # Include debug information
backend = "C"            # Compiler backend ("C")
static = false           # Statically link executable (true/false)
keep-intermediate = true # Keep intermediate files (e.g., .c files)
# target = "..."         # Default target triple for this profile

[profile.release]
# Settings for `jophet build --release` or `jophet run --release`
opt-level = 3
debug = false
backend = "C"
static = true
keep-intermediate = false
# target = "..."         # Default target triple for this profile
"#,
        name, project_type
    );
    fs::write(root.join("Jophet.toml"), toml_content)?;

    // Write the appropriate sample source file.
    if is_bin {
        fs::write(src_dir.join("main.jophet"), HELLO_WORLD_BIN.trim())?;
        diagnostics::print_creating("binary", name);
    } else {
        fs::write(src_dir.join("lib.jophet"), EXAMPLE_LIB.trim())?;
        diagnostics::print_creating("library", name);
    }

    Ok(())
}

/// The main entry point for the `new` command.
///
/// It checks if it's being run inside an existing Jophet project.
/// - If outside a project, it creates a new standalone project directory.
/// - If inside a project, it creates a new library package as a subdirectory,
///   ensuring it is a complete package with its own `Jophet.toml`. This lays
///   the groundwork for future workspace/multi-package support.
///
/// # Arguments
/// * `name` - The name of the new project or package.
/// * `is_bin` - `true` if creating a binary (application) project, `false` for a library.
pub fn handle_new(name: &str, is_bin: bool) -> Result<(), Box<dyn Error>> {
    // Check if we are inside an existing Jophet project.
    if let Some(root_path) = config::find_project_root() {
        if is_bin {
            // Disallow creating nested binary packages, as this is confusing.
            // Workspaces typically have one or more binaries at the top level.
            return Err(format!(
                "cannot create a binary package inside of an existing package `{}`",
                root_path.display()
            )
            .into());
        }

        // Create the new library package inside the current project's root directory.
        println!(
            "Creating library `{}` as a member of the current package...",
            name
        );
        let lib_root = Path::new(name);
        create_package(lib_root, name, false)?;
    } else {
        // We are not in a project, so create a new standalone project.
        let root = Path::new(name);
        create_package(root, name, is_bin)?;
    }

    Ok(())
}