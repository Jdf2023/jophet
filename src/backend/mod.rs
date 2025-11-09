// src/backend/mod.rs
//! The compiler backend interface and selection logic.
//!
//! This module defines the generic `Backend` trait, which all compiler backends
//! must implement. It provides a common interface for the build process to interact with
//! different code generators (e.g., C, LLVM, etc.). It also contains the logic for
//! selecting and instantiating a specific backend based on configuration or command-line
//! flags.

use crate::commands::build::BuildArtifact;
use crate::core::ast::typed::{TypedErrorDef, TypedProgram};
use crate::core::semantic_analyzer::ModuleScope;
use std::collections::HashMap;
use std::error::Error;
use std::path::PathBuf;

/// The C backend module.
pub mod c;

/// An enum to categorize the different kinds of files a backend can produce.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OutputFileType {
    /// A primary source file to be compiled (e.g., `.c`, `.ll`).
    Source,
    /// A header or interface file to be installed for other projects to use.
    PublicHeader,
    /// Any other file the backend needs to produce (e.g., runtime files).
    Auxiliary,
}

/// Information about the target architecture for compilation.
#[derive(Debug, Clone)]
pub struct TargetInfo {
    /// The target triple string (e.g., "x86_64-pc-windows-msvc").
    pub triple: String,
    /// The width of a pointer on the target architecture, in bits (e.g., 32 or 64).
    pub pointer_width: u8,
}

/// A constant array defining all target triples supported by the compiler.
/// This is the single source of truth for target information.
const SUPPORTED_TARGETS: &[(&str, u8)] = &[
    ("x86_64-pc-windows-msvc", 64),
    ("x86_64-unknown-linux-gnu", 64),
    ("x86_64-apple-darwin", 64),
    ("i686-pc-windows-gnu", 32),
    ("armv7-unknown-linux-gnueabihf", 32),
    ("aarch64-unknown-linux-gnu", 64),
    ("aarch64-apple-darwin", 64),
];

/// Returns a list of all supported target triple strings.
/// This is used to implement the `--list-targets` CLI flag.
pub fn get_supported_targets() -> Vec<String> {
    SUPPORTED_TARGETS
        .iter()
        .map(|(triple, _)| triple.to_string())
        .collect()
}

/// Resolves a target triple string into a structured `TargetInfo`.
///
/// This function contains the compiler's knowledge of different architectures. It maps
/// a standard target triple to its essential properties, like pointer width.
///
/// # Returns
/// A `Result` containing the `TargetInfo` on success, or an error string if the
/// target triple is not supported.
pub fn resolve_target_info(triple: &str) -> Result<TargetInfo, String> {
    for &(supported_triple, width) in SUPPORTED_TARGETS {
        if supported_triple == triple {
            return Ok(TargetInfo {
                triple: triple.to_string(),
                pointer_width: width,
            });
        }
    }
    Err(format!("Unsupported target triple: '{}'", triple))
}


/// The collection of files produced by a backend for a single compilation unit.
/// The `HashMap` keys are the desired filenames (e.g., "main.c", "mylib.h").
pub type BackendOutput = HashMap<OutputFileType, HashMap<String, String>>;

/// A struct containing all necessary information for a toolchain to perform compilation.
pub struct CompileOptions {
    pub project_name: String,
    pub is_lib: bool,
    pub is_release: bool,
    pub source_files: Vec<PathBuf>,
    pub final_artifact_path: PathBuf,
    pub jophet_lib_dir: PathBuf,
    pub jophet_include_dir: PathBuf,
    pub temp_build_dir: PathBuf,
    pub target_dir: PathBuf,
    pub dependencies: Vec<String>,
    pub shared_deps_dir: PathBuf,
    pub needs_python_runtime: bool,
    pub static_build: bool,
    /// Information about the target architecture for this compilation.
    pub target_info: TargetInfo,
}

/// A trait representing the toolchain for a specific backend (e.g., a C compiler, an LLVM toolchain).
/// A toolchain is responsible for taking generated source files and compiling them into a native artifact.
pub trait Toolchain {
    /// Compiles the source files into a final executable or library.
    fn compile(&self, options: CompileOptions) -> Result<BuildArtifact, Box<dyn Error>>;
}

/// A trait representing a compiler backend.
///
/// A backend is responsible for the final stage of compilation: taking the semantically
/// analyzed and type-checked Abstract Syntax Tree (`TypedProgram`) and transforming it
/// into a set of source files for a specific target representation.
pub trait Backend {
    /// Processes a typed AST, producing a map of source files for the target representation.
    ///
    /// # Arguments
    ///
    /// * `ast` - A reference to the fully analyzed `TypedProgram`.
    /// * `public_scope` - The public API of the current program, used for generating headers.
    /// * `source` - The original source code string, used for creating source maps.
    /// * `module_doc_comment` - The optional documentation for the entire module.
    /// * `filename` - The name of the original source file.
    /// * `lib_name` - The name of the library being built, if applicable.
    /// * `is_lib` - A boolean indicating if the target artifact is a library.
    /// * `imported_modules` - A map containing the analyzed public scopes of all imported modules.
    /// * `all_error_defs` - A complete list of all error definitions (user-defined and built-in).
    /// * `needs_python_runtime` - A flag indicating if the Python FFI runtime should be included.
    ///
    /// # Returns
    ///
    /// A `Result` containing either:
    /// - `Ok(BackendOutput)`: A structured map of all generated files, categorized by type.
    /// - `Err(Box<dyn Error>)`: An error if the code generation process fails.
    fn process(
        &self,
        ast: &TypedProgram,
        public_scope: &ModuleScope,
        source: &str,
        module_doc_comment: &Option<String>,
        filename: &str,
        lib_name: &str,
        is_lib: bool,
        imported_modules: &HashMap<String, ModuleScope>,
        all_error_defs: &[TypedErrorDef],
        needs_python_runtime: bool,
    ) -> Result<BackendOutput, Box<dyn Error>>;

    /// Returns the toolchain associated with this backend.
    fn get_toolchain(&self) -> Box<dyn Toolchain>;
}

/// An enum representing the available backend types.
/// This can be expanded in the future to support more backends.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    /// The C language backend.
    #[value(name = "C")]
    C,
}

/// Factory function to get an instance of a specific backend.
///
/// This function abstracts the creation of backend instances. The build process
/// calls this function to get the appropriate backend implementation based on the
/// desired `BackendType`.
///
/// # Arguments
///
/// * `backend_type` - The enum variant specifying which backend to create.
///
/// # Returns
///
/// A `Box<dyn Backend>` containing an instance of the requested backend.
pub fn get_backend(backend_type: BackendType) -> Box<dyn Backend> {
    match backend_type {
        BackendType::C => Box::new(c::CBackend),
    }
}