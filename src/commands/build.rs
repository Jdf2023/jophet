// src/commands/build.rs
//! Handles the `jophet build` command.
//!
//! This module orchestrates the entire compilation process, from loading the project
//! configuration to invoking the backend and finally calling the backend's toolchain
//! to compile the generated code into a native artifact. It now supports recursive
//! builds to handle local, source-based dependencies. It is updated to handle the
//! collection of multiple errors from the frontend and enforces conventional entry
//! points for packages (`src/main.jophet` for binaries, `src/lib.jophet` for libraries).

use crate::backend::{self, BackendType, CompileOptions, OutputFileType};
use crate::config::{load_config, load_config_for_path};
use crate::core;
use crate::diagnostics;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// A custom error type to wrap multiple `JophetError`s, allowing it to be
/// downcast from a `Box<dyn Error>`.
#[derive(Debug)]
pub struct MultipleErrors(pub Vec<diagnostics::errors::JophetError>);

impl std::fmt::Display for MultipleErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Compilation failed with {} error(s)", self.0.len())
    }
}
impl Error for MultipleErrors {}

/// Represents the final output of a successful build.
#[derive(Debug, Clone)]
pub struct BuildArtifact {
    /// The path to the generated executable or library.
    pub path: PathBuf,
    /// A flag indicating whether the artifact is a library.
    pub is_lib: bool,
    /// Path to the generated .jophet-meta file (for libraries).
    pub meta_path: Option<PathBuf>,
    /// Path to the generated public header file (for libraries).
    pub header_path: Option<PathBuf>,
}

/// The main entry point for the `build` command. This is a wrapper around `build_package`.
///
/// It determines the root project and the shared target/dependency directories and then
/// calls `build_package` to perform the actual compilation. It is now the central
/// point for resolving build configuration from both CLI flags and `Jophet.toml`. It
/// also resolves the target architecture for cross-compilation.
pub fn handle_build(
    is_release: bool,
    is_installing: bool,
    keep_intermediate_cli: bool,
    backend_type_cli: Option<BackendType>,
    static_build_cli: bool,
    target_cli: Option<String>,
) -> Result<BuildArtifact, Box<dyn Error>> {
    let (config, project_root) = load_config()?;
    let profile_name = if is_release { "release" } else { "debug" };
    let shared_deps_dir = project_root
        .join("target")
        .join(profile_name)
        .join("deps");
    fs::create_dir_all(&shared_deps_dir)?;

    let is_lib = config.package.r#type == "lib";

    let profile_config = if is_release {
        &config.profile.release
    } else {
        &config.profile.dev
    };

    // --- Apply Precedence Logic ---
    // 1. Command-line flag (highest priority)
    // 2. Jophet.toml profile setting
    // 3. Sensible default (lowest priority)

    let backend_type = backend_type_cli.unwrap_or_else(|| {
        profile_config
            .backend
            .as_deref()
            .and_then(|s| match s.to_lowercase().as_str() {
                "c" => Some(BackendType::C),
                _ => None,
            })
            .unwrap_or(BackendType::C) // Default
    });

    let keep_intermediate = if keep_intermediate_cli {
        true
    } else {
        profile_config.keep_intermediate.unwrap_or(false)
    };

    let static_build = if static_build_cli {
        true
    } else {
        profile_config.static_build.unwrap_or(false)
    };

    // Determine the target triple with full precedence: CLI > TOML > Host Default
    let target_triple = target_cli.or(profile_config.target.clone()).unwrap_or_else(|| env!("JOPHET_HOST_TRIPLE").to_string());
    let target_info = backend::resolve_target_info(&target_triple)?;

    build_package(
        &project_root,
        is_release,
        is_installing,
        keep_intermediate, // <-- resolved value
        backend_type,      // <-- resolved value
        &shared_deps_dir,
        // When building the main package, the root is the project path itself.
        &project_root,
        is_lib, // Pass the explicit package type from the config.
        false,  // The main build is never in REPL mode
        static_build,      // <-- resolved value
        target_info,
    )
}

/// Builds a single Jophet package, which can be a main project or a local dependency.
/// This function now relies on the `is_lib` parameter to determine the artifact type,
/// rather than inferring it from the path. It also enforces conventional entry points:
/// `src/main.jophet` for binaries and `src/lib.jophet` or `<dir>/lib.jophet` for libraries.
///
/// This function coordinates the following steps for a given package path:
/// 1. Loads the `Jophet.toml` configuration for the specified package if it's a directory.
/// 2. Sets up the temporary build directory inside the main project's target directory.
/// 3. Runs the compiler frontend (lexer, parser, semantic analyzer) to get a typed AST.
///    The analyzer is now aware of the shared dependency directory for resolving other local dependencies.
/// 4. If building a library, it saves the public API metadata and header to the shared dependency directory.
/// 5. Invokes the selected backend to generate C source files.
/// 6. Gets the appropriate toolchain from the backend.
/// 7. Invokes the toolchain's `compile` method to produce the native artifact (e.g., `.a`, `.lib`, or executable).
/// 8. Returns a `BuildArtifact` describing the final output on success, or an error.
pub fn build_package(
    package_path: &Path, // The path to the package being built (e.g., myProject/ or myProject/src/myLib)
    is_release: bool,
    is_installing: bool,
    keep_intermediate: bool,
    backend_type: BackendType,
    shared_deps_dir: &Path,
    project_root: &Path, // The true root of the entire project (e.g., myProject)
    is_lib: bool,        // Explicitly tell the function whether to build a library or executable.
    is_repl_mode: bool,
    static_build: bool,
    target_info: backend::TargetInfo,
) -> Result<BuildArtifact, Box<dyn Error>> {
    let start_time = Instant::now();

    // Determine the project name and entry point based on the path.
    // The `is_lib` parameter passed to this function is the source of truth for the artifact type.
    let (config, project_name_str, entry_point_file) = if package_path.is_dir() {
        let (config, name) = if package_path.join("Jophet.toml").exists() {
            let config = load_config_for_path(package_path)?;
            let name = config.package.name.clone();
            (Some(config), name)
        } else {
            let name = package_path.file_name().unwrap().to_str().unwrap().to_string();
            (None, name)
        };

        // Determine entry point based on package convention.
        let entry_point = if is_lib {
            // For a directory-based library, the entry point can be `src/lib.jophet`
            // (for the main project) or `<dir>/lib.jophet` (for a local module).
            let src_lib_path = package_path.join("src").join("lib.jophet");
            let direct_lib_path = package_path.join("lib.jophet");
            if src_lib_path.exists() {
                src_lib_path
            } else {
                direct_lib_path
            }
        } else {
            // Binaries always have their entry point at `src/main.jophet`.
            package_path.join("src").join("main.jophet")
        };
        (config, name, entry_point)
    } else if package_path.is_file() {
        let name = package_path.file_stem().unwrap().to_str().unwrap().to_string();
        (None, name, package_path.to_path_buf())
    } else {
        return Err(format!("Build target path '{}' is not a valid file or directory.", package_path.display()).into());
    };

    let project_name = &project_name_str;

    if !entry_point_file.exists() {
        return Err(format!(
            "Project entry point not found at '{}'",
            entry_point_file.display()
        )
        .into());
    }

    // Set up build directories. All temporary builds happen under the main project's target dir.
    let target_dir = shared_deps_dir.parent().unwrap().to_path_buf();
    let temp_build_dir = target_dir.join("build").join(project_name);

    if temp_build_dir.exists() {
        fs::remove_dir_all(&temp_build_dir)?;
    }
    fs::create_dir_all(&temp_build_dir)?;

    // Suppress the "Compiling" message for the REPL's internal library.
    if project_name != "repl_lib" {
        if let Some(ref cfg) = config {
            diagnostics::print_compiling(project_name, &cfg.package.version);
        }
    }

    let source_code = fs::read_to_string(&entry_point_file)?;

    // Run the compiler frontend. Crucially, pass the overall project_root.
    let analysis_result = core::run_frontend(
        &source_code,
        project_root.to_path_buf(), // Pass the true project root
        entry_point_file.clone(),
        shared_deps_dir,
        is_release,
        keep_intermediate,
        backend_type,
        is_repl_mode,
        &target_info,
    )
    .map_err(|errors| -> Box<dyn Error> {
        // Wrap the Vec<JophetError> in our custom error type.
        Box::new(MultipleErrors(errors))
    })?;
    let typed_ast = analysis_result.typed_program;

    // For libraries being installed globally, serialize their public API to `~/.jophet/meta`.
    if is_lib && is_installing {
        let home_dir = home::home_dir().ok_or("Could not find home directory.")?;
        let jophet_meta_dir = home_dir.join(".jophet").join("meta");
        fs::create_dir_all(&jophet_meta_dir)?;
        let meta_path = jophet_meta_dir.join(format!("{}.jophet-meta", project_name));
        let meta_content = serde_json::to_string_pretty(&analysis_result.public_scope)?;
        fs::write(meta_path, meta_content)?;
    }

    // Run the compiler backend to generate source files.
    let backend = backend::get_backend(backend_type);
    let entry_filename = entry_point_file.file_name().unwrap().to_str().unwrap();
    let backend_output = backend.process(
        &typed_ast,
        &analysis_result.public_scope,
        &source_code,
        &analysis_result.module_doc_comment,
        entry_filename,
        project_name,
        is_lib,
        &analysis_result.imported_modules,
        &analysis_result.all_error_defs,
        analysis_result.needs_python_runtime,
    )?;

    // Write the generated files to the temporary build directory.
    let mut source_files_to_compile = Vec::new();

    // Handle SOURCE files: these must be written AND passed to the compiler.
    if let Some(source_files) = backend_output.get(&OutputFileType::Source) {
        for (name, content) in source_files {
            let path = temp_build_dir.join(name);
            fs::write(&path, content)?;
            source_files_to_compile.push(path);
        }
    }

    // Handle AUXILIARY files: these must be written but are NOT passed to the compiler.
    // This is for files like headers (`runtime.h`).
    if let Some(aux_files) = backend_output.get(&OutputFileType::Auxiliary) {
        for (name, content) in aux_files {
            let path = temp_build_dir.join(name);
            fs::write(&path, content)?;
        }
    }

    let mut meta_path = None;
    let mut header_path = None;

    // For libraries (both local and for installation), write metadata and headers.
    if is_lib {
        // Metadata for local dependencies goes into the shared deps directory.
        let meta_dest_path = shared_deps_dir.join(format!("{}.jophet-meta", project_name));
        let meta_content = serde_json::to_string_pretty(&analysis_result.public_scope)?;
        fs::write(&meta_dest_path, meta_content)?;
        meta_path = Some(meta_dest_path);

        // Public headers for local dependencies also go into the shared deps directory.
        if let Some(headers) = backend_output.get(&OutputFileType::PublicHeader) {
            if let Some((name, content)) = headers.iter().next() {
                let header_dest_path = shared_deps_dir.join(name);
                fs::write(&header_dest_path, content)?;
                header_path = Some(header_dest_path);
            }
        }
    }

    // Determine the final output path and name based on the TARGET.
    let is_windows_target = target_info.triple.contains("windows");
    let final_artifact_path = if is_lib {
        let lib_name = if is_windows_target {
            format!("{}.lib", project_name)
        } else {
            format!("lib{}.a", project_name)
        };
        // Local dependency libraries are placed in the shared deps directory.
        shared_deps_dir.join(lib_name)
    } else {
        // The main executable goes in the main target directory.
        let mut path = target_dir.join(project_name);
        if is_windows_target {
            path.set_extension("exe");
        }
        path
    };

    let home_dir = home::home_dir().ok_or("Could not find home directory.")?;
    let jophet_lib_dir = home_dir.join(".jophet").join("lib");
    let jophet_include_dir = home_dir.join(".jophet").join("include");

    let mut all_deps = analysis_result.linked_libs;
    if let Some(ref cfg) = config {
        for dep_name in cfg.dependencies.keys() {
            if !all_deps.contains(dep_name) {
                all_deps.push(dep_name.clone());
            }
        }
    }

    let toolchain = backend.get_toolchain();
    let compile_options = CompileOptions {
        project_name: project_name.to_string(),
        is_lib,
        is_release,
        source_files: source_files_to_compile,
        final_artifact_path: final_artifact_path.clone(),
        jophet_lib_dir,
        jophet_include_dir,
        temp_build_dir: temp_build_dir.clone(),
        target_dir,
        dependencies: all_deps,
        shared_deps_dir: shared_deps_dir.to_path_buf(),
        needs_python_runtime: analysis_result.needs_python_runtime,
        static_build,
        target_info,
    };

    // This returns a "raw" build artifact from the toolchain.
    let raw_artifact = toolchain.compile(compile_options)?;

    // Suppress the "Finished" message for the REPL's internal library.
    if project_name != "repl_lib" {
        if config.is_some() {
            let duration = start_time.elapsed();
            let profile_name = if is_release { "release" } else { "debug" };
            diagnostics::print_finished(profile_name, duration);
        }
    }

    if !keep_intermediate {
        if temp_build_dir.exists() {
            fs::remove_dir_all(&temp_build_dir)?;
        }
    }

    // Return the enriched BuildArtifact with metadata and header paths.
    Ok(BuildArtifact {
        path: raw_artifact.path,
        is_lib: raw_artifact.is_lib,
        meta_path,
        header_path,
    })
}