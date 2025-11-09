// src/commands/document.rs
//! Handles the `jophet doc` command.
//!
//! This module orchestrates the documentation generation process. It runs the
//! compiler frontend to get a fully analyzed view of the project, then invokes
//! the `HtmlGenerator` to produce the final HTML output.

use crate::backend::{self, BackendType};
use crate::commands::build::MultipleErrors;
use crate::config;
use crate::core;
use crate::diagnostics;
use crate::docs;
use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

/// The main entry point for the `document` command.
///
/// It runs the compiler frontend to gather all necessary information and then passes
/// it to the documentation generator. The final HTML file is saved in `target/doc/`.
pub fn handle_document(backend_type: BackendType) -> Result<(), Box<dyn Error>> {
    let start_time = Instant::now();
    let (config, project_root) = config::load_config()?;

    diagnostics::print_documenting(&config.package.name, &config.package.version);

    let is_lib = config.package.r#type == "lib";
    let entry_point_file = if is_lib {
        project_root.join("src").join("lib.jophet")
    } else {
        project_root.join("src").join("main.jophet")
    };

    if !entry_point_file.exists() {
        return Err(format!(
            "Project entry point not found at '{}'",
            entry_point_file.display()
        )
        .into());
    }

    // Set up a dummy deps dir as we don't need to build dependencies for docs.
    let shared_deps_dir = project_root.join("target/doc/deps");
    fs::create_dir_all(&shared_deps_dir)?;

    let source_code = fs::read_to_string(&entry_point_file)?;

    // For documentation, we don't need a specific target. We can default to the host.
    let host_triple = env!("JOPHET_HOST_TRIPLE").to_string();
    let target_info = backend::resolve_target_info(&host_triple)?;

    // Run the compiler frontend to get the full analysis result.
    let analysis_result = core::run_frontend(
        &source_code,
        project_root.clone(),
        entry_point_file,
        &shared_deps_dir,
        false, // Not a release build
        false, // Don't keep intermediate files
        backend_type,
        false, // Document command is not in REPL mode
        &target_info,
    )
    .map_err(|errors| -> Box<dyn Error> { Box::new(MultipleErrors(errors)) })?;

    // Generate the HTML content.
    let html_content =
        docs::html_generator::generate_html(&config.package.name, &analysis_result)?;

    // Save the final HTML file.
    let doc_dir = project_root.join("target/doc");
    fs::create_dir_all(&doc_dir)?;
    let output_path = doc_dir.join(format!("{}.html", config.package.name));
    fs::write(&output_path, html_content)?;

    let duration = start_time.elapsed();
    diagnostics::print_finished_doc(&output_path, duration);

    Ok(())
}