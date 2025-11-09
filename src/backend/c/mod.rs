// src/backend/c/mod.rs
//! The C backend for the Jophet compiler.
//!
//! This module contains all the logic required to transpile a Jophet typed Abstract
//! Syntax Tree (AST) into C source code. It includes the `CBackend` struct, which
//! implements the generic `Backend` trait, and the `Generator` struct, which manages
//! the state of the C code generation process. It now also handles converting Jophet
//! doc comments into C Doxygen-style comments. It also defines the `CToolchain`,
//! which is responsible for compiling the generated C code into a native artifact.

use crate::backend::{BackendOutput, CompileOptions, OutputFileType, Toolchain};
use crate::commands::build::BuildArtifact;
use crate::config;
use crate::core::ast::typed::*;
use crate::core::semantic_analyzer::ModuleScope;
use crate::diagnostics::errors::JophetError;
use indexmap::IndexSet;
use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

mod declarations;
pub mod expressions;
mod statements;
mod types;

/// A simple utility to map byte offsets in source code to line numbers.
/// This is used for providing better error messages in runtime panics.
pub struct SourceMap {
    /// The filename of the source code.
    filename: String,
    /// A vector where each element is the starting byte offset of a line.
    line_starts: Vec<usize>,
}

impl SourceMap {
    /// Creates a new `SourceMap` from a source code string and its filename.
    pub fn new(source: &str, filename: &str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        SourceMap {
            filename: filename.to_string(),
            line_starts,
        }
    }

    /// Returns the 1-based line number for a given byte offset.
    fn line_for_byte(&self, byte_offset: usize) -> usize {
        self.line_starts
            .iter()
            .rposition(|&start| start <= byte_offset)
            .map_or(1, |i| i + 1)
    }

    /// Returns the filename.
    fn filename(&self) -> &str {
        &self.filename
    }
}

/// A helper function to find a command in the system's PATH.
fn find_in_path(command: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths).find_map(|dir| {
            let full_path = dir.join(command);
            if full_path.is_file() {
                Some(full_path)
            } else if cfg!(windows) {
                // On Windows, check for .exe extension
                let full_path_exe = full_path.with_extension("exe");
                if full_path_exe.is_file() {
                    Some(full_path_exe)
                } else {
                    None
                }
            } else {
                None
            }
        })
    })
}

/// Discovers the C compiler to use for the host system and identifies its family.
/// It prioritizes the `CC` environment variable, then searches for common
/// compiler names in the system's PATH.
/// Returns a tuple of (compiler_path, is_msvc).
fn discover_host_compiler() -> Result<(PathBuf, bool), String> {
    if let Ok(compiler_path) = env::var("CC") {
        let path = PathBuf::from(&compiler_path);
        if path.is_file() {
            let is_msvc = path.file_stem().map_or(false, |s| s == "cl");
            return Ok((path, is_msvc));
        }
    }

    if cfg!(target_env = "msvc") {
        if let Some(path) = find_in_path("cl") {
            return Ok((path, true));
        }
    }

    if let Some(path) = find_in_path("gcc") {
        return Ok((path, false));
    }

    if let Some(path) = find_in_path("clang") {
        return Ok((path, false));
    }

    Err("Could not find a C compiler (cl, gcc, or clang) in your PATH. Please install a C compiler or set the CC environment variable.".to_string())
}

/// Discovers the C cross-compiler for a given target triple from environment variables.
/// Returns a tuple of (compiler_path, is_msvc).
fn discover_cross_compiler(target_triple: &str) -> Option<(PathBuf, bool)> {
    let env_var_name = format!("CC_{}", target_triple.replace('-', "_"));
    env::var_os(&env_var_name).map(|p| {
        let path = PathBuf::from(p);
        let is_msvc = path.file_stem().map_or(false, |s| s == "cl");
        (path, is_msvc)
    })
}

/// The toolchain for the C backend, responsible for invoking a C compiler.
pub struct CToolchain;

/// Discovers the necessary C compiler flags (for includes) and linker flags
/// (for libraries) to link against the system's Python installation.
///
/// This function provides a robust, cross-platform way to find the Python
/// development files. On POSIX systems, it first tries `pkg-config`, the standard
/// tool for this purpose. If that fails, it falls back to directly querying the
/// `python3` interpreter using its `sysconfig` module. On Windows, it tries the
/// official `py.exe` launcher first to bypass Microsoft Store shims, then falls
/// back to `python.exe`.
///
/// # Arguments
/// * `is_msvc` - A boolean indicating whether the detected C compiler is MSVC-like.
///
/// # Returns
/// A `Result` containing a tuple `(cflags, ldflags)` on success.
/// - `cflags`: A `Vec<String>` of compiler flags (e.g., `-I/path/to/python/include`).
/// - `ldflags`: A `Vec<String>` of linker flags (e.g., `-L/path/to/python/lib -lpython3.10`).
fn discover_python_paths(is_msvc: bool) -> Result<(Vec<String>, Vec<String>), String> {
    // Non-Windows (Linux, macOS) strategy:
    // 1. Try pkg-config (the standard way).
    // 2. Fall back to querying the python3 interpreter via sysconfig.
    #[cfg(not(windows))]
    {
        // 1. Try pkg-config
        if let Ok(output) = Command::new("pkg-config").args(["--cflags", "--libs", "python3"]).output() {
            if output.status.success() {
                let flags_str = String::from_utf8_lossy(&output.stdout);
                let flags: Vec<String> = flags_str.split_whitespace().map(String::from).collect();
                let cflags: Vec<String> = flags.iter().filter(|f| f.starts_with("-I")).cloned().collect();
                let ldflags: Vec<String> = flags.iter().filter(|f| !f.starts_with("-I")).cloned().collect();
                if !cflags.is_empty() && !ldflags.is_empty() {
                    return Ok((cflags, ldflags));
                }
            }
        }

        // 2. Fallback to python3 interpreter
        if let Ok(output) = Command::new("python3")
            .arg("-c")
            .arg("import sysconfig; print(sysconfig.get_path('include'), end='\\n'); print(sysconfig.get_config_var('LIBDIR'), end='\\n'); print(sysconfig.get_config_var('LDLIBRARY'))")
            .output()
        {
            if output.status.success() {
                let paths = String::from_utf8_lossy(&output.stdout);
                let mut lines = paths.lines();
                if let (Some(include_path), Some(lib_path), Some(lib_file)) = (lines.next(), lines.next(), lines.next()) {
                    let cflags = vec![format!("-I{}", include_path.trim())];
                    let mut ldflags = vec![format!("-L{}", lib_path.trim())];
                    // Parse 'libpython3.10.so' into '-lpython3.10'
                    if let Some(lib_name) = lib_file.trim().strip_prefix("lib").and_then(|s| s.split('.').next()) {
                        ldflags.push(format!("-l{}", lib_name));
                    }
                    return Ok((cflags, ldflags));
                }
            }
        }
        
        Err("Could not find Python 3 development files. Please ensure python3-dev (or equivalent) and pkg-config are installed.".to_string())
    }
    
    // Windows strategy:
    // Try `py.exe` first, then `python.exe` to avoid Microsoft Store stubs.
    #[cfg(windows)]
    {
        // This python script is more robust for Windows process creation, avoiding shell quoting issues.
        const PYTHON_SCRIPT: &str = r#"
import sysconfig, sys
sys.stdout.write(sysconfig.get_path('include') + '\n')
sys.stdout.write(sysconfig.get_config_var('LIBDIR') + '\n')
sys.stdout.write(sysconfig.get_config_var('py_version_nodot') + '\n')
"#;
        
        let commands_to_try = ["py", "python"];
        for cmd_name in commands_to_try {
             let output = Command::new(cmd_name)
                .arg("-c")
                .arg(PYTHON_SCRIPT)
                .output();

            if let Ok(py_output) = output {
                if py_output.status.success() {
                    let paths = String::from_utf8_lossy(&py_output.stdout);
                    let mut lines = paths.lines();
                    if let (Some(include_path), Some(lib_path), Some(version)) = (lines.next(), lines.next(), lines.next()) {
                        // Check for empty strings which can happen if the Microsoft Store stub runs
                        if include_path.trim().is_empty() || lib_path.trim().is_empty() {
                            continue;
                        }

                        if is_msvc {
                            let cflags = vec![format!("/I{}", include_path.trim())];
                            let ldflags = vec![
                                format!("/LIBPATH:{}", lib_path.trim()),
                                format!("python{}.lib", version.trim()),
                            ];
                            return Ok((cflags, ldflags));
                        } else {
                            // Generate GCC/Clang style flags, even on Windows
                            let cflags = vec![format!("-I{}", include_path.trim())];
                            let ldflags = vec![
                                format!("-L{}", lib_path.trim()),
                                format!("-lpython{}", version.trim()),
                            ];
                            return Ok((cflags, ldflags));
                        }
                    }
                }
            }
        }

        Err("Failed to get Python configuration using 'py' or 'python'. Please ensure a full Python distribution is installed and available in your PATH, not the Microsoft Store shim.".to_string())
    }
}

impl Toolchain for CToolchain {
    /// Compiles the source files into a final executable or library.
    ///
    /// This function orchestrates the invocation of the host C compiler (GCC, Clang, or MSVC).
    /// It assembles the necessary compiler flags, include paths, and library linkage information.
    /// It now gracefully handles contexts without a `Jophet.toml` (like the REPL) by using
    /// default build profiles. It also handles static linking via the `static_build` option.
    /// It correctly selects a cross-compiler based on the target triple if one is configured.
    fn compile(
        &self,
        options: CompileOptions,
    ) -> Result<crate::commands::build::BuildArtifact, Box<dyn Error>> {
        let (host_compiler_path, host_is_msvc) = discover_host_compiler()?;

        // --- CORRECTED COMPILER SELECTION LOGIC ---
        // Determine which compiler to use and its family (MSVC vs GCC/Clang).
        let (compiler_path, is_msvc) = if options.target_info.triple == env!("JOPHET_HOST_TRIPLE") {
            // Target is the host, use the default host compiler.
            (host_compiler_path, host_is_msvc)
        } else {
            // Target is different, try to find a configured cross-compiler.
            discover_cross_compiler(&options.target_info.triple).ok_or_else(|| {
                JophetError::BuildFailed {
                    reason: format!(
                        "Cross-compiler for target '{}' not found. Help: Set the CC_{} environment variable to the path of your C cross-compiler (e.g., 'x86_64-linux-gnu-gcc').",
                        options.target_info.triple,
                        options.target_info.triple.replace('-', "_")
                    )
                }
            })?
        };
        
        // --- FIX: Gracefully handle the absence of Jophet.toml ---
        // Try to load config. If it fails (e.g., in a standalone REPL), use default profiles.
        let profiles = match config::load_config() {
            Ok((config, _)) => config.profile,
            Err(_) => config::Profiles::default(),
        };

        let profile_config = if options.is_release {
            &profiles.release
        } else {
            &profiles.dev
        };

        if options.is_lib {
            // --- MANUAL LIBRARY COMPILATION ---
            let mut object_files = Vec::new();
            for source_file in &options.source_files {
                let mut cmd = Command::new(&compiler_path);

                // Add include paths
                if is_msvc {
                    cmd.arg(format!("/I{}", options.temp_build_dir.display()));
                    cmd.arg(format!("/I{}", options.shared_deps_dir.display()));
                    if options.jophet_include_dir.exists() {
                        cmd.arg(format!("/I{}", options.jophet_include_dir.display()));
                    }
                } else {
                    cmd.arg(format!("-I{}", options.temp_build_dir.display()));
                    cmd.arg(format!("-I{}", options.shared_deps_dir.display()));
                    if options.jophet_include_dir.exists() {
                        cmd.arg(format!("-I{}", options.jophet_include_dir.display()));
                    }
                }

                if options.needs_python_runtime {
                    let (cflags, _) = discover_python_paths(is_msvc).map_err(|e| Box::new(JophetError::BuildFailed { reason: e }))?;
                    for flag in cflags {
                        cmd.arg(flag);
                    }
                }

                // Add profile flags
                if is_msvc {
                    cmd.arg("/nologo");
                    cmd.arg("/W3");
                    match profile_config.opt_level.unwrap_or(0) {
                        0 => cmd.arg("/Od"),
                        1 => cmd.arg("/O1"),
                        2 => cmd.arg("/O2"),
                        _ => cmd.arg("/Ox"),
                    };
                    if profile_config.debug.unwrap_or(true) {
                        cmd.arg("/Zi");
                    }
                } else {
                    cmd.arg("-Wall");
                    cmd.arg(format!("-O{}", profile_config.opt_level.unwrap_or(0)));
                    if profile_config.debug.unwrap_or(true) {
                        cmd.arg("-g");
                    }
                }

                // Compile to an object file (-c flag)
                let object_file = options.temp_build_dir.join(source_file.file_name().unwrap()).with_extension(if is_msvc { "obj" } else { "o" });
                if is_msvc {
                    cmd.arg("/c");
                    cmd.arg(format!("/Fo:{}", object_file.display()));
                } else {
                    cmd.arg("-c");
                    cmd.arg("-o").arg(&object_file);
                }
                cmd.arg(source_file);

                let output = cmd.output()?;
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(Box::new(JophetError::BuildFailed {
                        reason: format!(
                            "The C compiler failed to compile object file for library '{}'. Compiler error:\n{}",
                            options.project_name, stderr
                        ),
                    }));
                }
                object_files.push(object_file);
            }

            // Archive the object files into a static library
            let archiver = if is_msvc { find_in_path("lib").ok_or("Could not find MSVC archiver 'lib.exe'")? } else { find_in_path("ar").ok_or("Could not find archiver 'ar'")? };
            let mut cmd = Command::new(archiver);
            if is_msvc {
                cmd.arg(format!("/OUT:{}", options.final_artifact_path.display()));
            } else {
                cmd.arg("rcs");
                cmd.arg(&options.final_artifact_path);
            }
            cmd.args(&object_files);

            let output = cmd.output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Box::new(JophetError::BuildFailed {
                    reason: format!(
                        "The archiver failed to create library '{}'. Archiver error:\n{}",
                        options.project_name, stderr
                    ),
                }));
            }
        } else {
            // --- EXISTING EXECUTABLE COMPILATION ---
            let mut cmd = Command::new(&compiler_path);
            let mut python_ldflags = Vec::new();

            for path in &options.source_files {
                cmd.arg(path);
            }

            if is_msvc {
                cmd.arg(format!("/I{}", options.temp_build_dir.display()));
                cmd.arg(format!("/I{}", options.shared_deps_dir.display()));
                if options.jophet_include_dir.exists() {
                    cmd.arg(format!("/I{}", options.jophet_include_dir.display()));
                }
            } else {
                cmd.arg(format!("-I{}", options.temp_build_dir.display()));
                cmd.arg(format!("-I{}", options.shared_deps_dir.display()));
                if options.jophet_include_dir.exists() {
                    cmd.arg(format!("-I{}", options.jophet_include_dir.display()));
                }
            }
            
            if is_msvc {
                cmd.arg("/nologo");
                cmd.arg("/W3");
                match profile_config.opt_level.unwrap_or(0) {
                    0 => cmd.arg("/Od"),
                    1 => cmd.arg("/O1"),
                    2 => cmd.arg("/O2"),
                    _ => cmd.arg("/Ox"),
                };
                if profile_config.debug.unwrap_or(true) {
                    cmd.arg("/Zi");
                }
            } else {
                cmd.arg("-Wall");
                cmd.arg(format!("-O{}", profile_config.opt_level.unwrap_or(0)));
                if profile_config.debug.unwrap_or(true) {
                    cmd.arg("-g");
                }
            }

            // Add static linking flags if requested for an executable.
            if options.static_build && !options.is_lib {
                if is_msvc {
                    // For MSVC, use the static runtime library.
                    if options.is_release {
                        cmd.arg("/MT");
                    } else {
                        cmd.arg("/MTd");
                    }
                } else {
                    // For GCC/Clang, use the -static flag.
                    cmd.arg("-static");
                }
            }

            if options.needs_python_runtime {
                let (cflags, ldflags) = discover_python_paths(is_msvc).map_err(|e| Box::new(JophetError::BuildFailed { reason: e }))?;

                for flag in cflags {
                    cmd.arg(flag);
                }
                
                if is_msvc {
                    // Linker flags for MSVC must be passed after the /link argument
                    python_ldflags = ldflags;
                } else {
                    for flag in ldflags {
                        cmd.arg(flag);
                    }
                }
            }

            if is_msvc {
                cmd.arg(format!("/Fe:{}", options.final_artifact_path.display()));
                cmd.arg("/link");
                cmd.args(&python_ldflags); // Add Python linker flags
                if options.shared_deps_dir.exists() {
                    cmd.arg(format!("/LIBPATH:{}", options.shared_deps_dir.display()));
                }
                if options.jophet_lib_dir.exists() {
                    cmd.arg(format!("/LIBPATH:{}", options.jophet_lib_dir.display()));
                }
                for lib in &options.dependencies {
                    cmd.arg(format!("{}.lib", lib));
                }
                if profile_config.debug.unwrap_or(true) {
                    cmd.arg("/DEBUG");
                }
            } else {
                cmd.arg("-o").arg(&options.final_artifact_path);
                if options.shared_deps_dir.exists() {
                    cmd.arg(format!("-L{}", options.shared_deps_dir.display()));
                }
                if options.jophet_lib_dir.exists() {
                    cmd.arg(format!("-L{}", options.jophet_lib_dir.display()));
                }
                for lib in &options.dependencies {
                    cmd.arg(format!("-l{}", lib));
                }
                // Add `-lm` to link the math library on non-MSVC platforms
                cmd.arg("-lm");
            }

            let output = cmd.output()?;
            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(Box::new(JophetError::BuildFailed {
                    reason: format!("The C compiler failed to create an executable. Compiler output:\n---stdout---\n{}\n---stderr---\n{}", stdout, stderr),
                }));
            }
        }

        // Return a raw BuildArtifact; the calling function will enrich it.
        Ok(BuildArtifact {
            path: options.final_artifact_path.clone(),
            is_lib: options.is_lib,
            meta_path: None,
            header_path: None,
        })
    }
}

/// A struct representing the C backend.
/// It acts as the public entry point for the C code generation process.
pub struct CBackend;

impl crate::backend::Backend for CBackend {
    /// Processes a typed AST, producing a map of C source files.
    /// This is the implementation of the `Backend` trait for the C target.
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
    ) -> Result<BackendOutput, Box<dyn Error>> {
        let source_map = SourceMap::new(source, filename);
        let mut generator = Generator::new(source_map, is_lib);
        generator.generate_program(
            ast,
            public_scope,
            module_doc_comment,
            lib_name,
            imported_modules,
            filename,
            all_error_defs,
            needs_python_runtime,
        )
    }

    fn get_toolchain(&self) -> Box<dyn Toolchain> {
        Box::new(CToolchain)
    }
}

/// Manages the state and logic for generating C code from a Jophet AST.
pub struct Generator {
    /// The main output buffer, primarily used for the body of the `main` function.
    output: String,
    /// A buffer dedicated to storing C function definitions.
    function_defs: String,
    /// A buffer dedicated to storing C global variable definitions.
    global_defs: String,
    /// A set for storing C function prototypes. Using `IndexSet` preserves insertion order.
    function_prototypes: IndexSet<String>,
    /// A single, order-preserving set for all C `typedef`s (structs, enums, tuples, etc.).
    /// This ensures dependent types (like tuples used in structs) are defined before they are used.
    type_defs: IndexSet<String>,
    /// A map of generated helper functions for deep-cloning vectors.
    /// The key is the function name, the value is the C code.
    vector_clone_helpers: HashMap<String, String>,
    /// A map of generated helper functions for deep-deleting vectors.
    /// The key is the function name, the value is the C code.
    vector_delete_helpers: HashMap<String, String>,
    /// A map of generated helper functions for printing vectors of a specific type.
    vector_print_helpers: HashMap<String, String>,
    /// A set of generated helper function names for string-printing complex types.
    /// Used to avoid generating duplicate function definitions.
    sprint_helpers: HashSet<String>,
    /// A set of `(key_type, value_type)` tuples for which dictionary print functions
    /// need to be generated. Using a `HashSet` prevents duplicate generation.
    dictionaries_to_print: HashSet<(JophetType, JophetType)>,
    /// A map of generated C thunk functions for deleting dictionary items.
    /// The key is the mangled type name, the value is the generated function's C code.
    dictionary_delete_thunks: HashMap<String, String>,
    /// A map of generated C thunk functions for cloning dictionary items.
    /// The key is the mangled type name, the value is the generated function's C code.
    dictionary_clone_thunks: HashMap<String, String>,
    /// A set of aggregate types that need to be convertible to Python objects.
    /// Used to generate the `jophet_to_py_object_dispatcher` function.
    python_convertible_types: HashSet<JophetType>,
    /// A set of mangled names for C struct wrappers for array return types that have been generated.
    array_return_structs: HashSet<String>,
    /// A counter for generating unique names for temporary variables.
    temp_var_counter: usize,
    /// A stack to manage cleanup actions (e.g., auto-deletes) for nested scopes.
    /// Each `Vec<String>` holds the C code statements for cleanup in one scope.
    scope_cleanup_stack: Vec<Vec<String>>,
    /// A set of struct names that contain owned data and thus require a destructor.
    structs_with_destructors: HashSet<String>,
    /// A set of tagged union/error names that contain owned data and thus require a destructor.
    tagged_unions_with_destructors: HashSet<String>,
    /// The captured variables of the closure currently being compiled.
    current_closure_captures: HashSet<String>,
    /// The return type of the function currently being compiled.
    current_function_return_type: Option<JophetType>,
    /// A cache of typed struct definitions for use in code generation.
    struct_defs_cache: HashMap<String, TypedStructDef>,
    /// A cache of typed enum definitions for use in code generation.
    enum_defs_cache: HashMap<String, TypedEnumDef>,
    /// A cache of typed tagged union definitions for use in code generation.
    tagged_union_defs_cache: HashMap<String, TypedTaggedUnionDef>,
    /// A cache of typed error definitions for use in code generation.
    error_defs_cache: HashMap<String, TypedErrorDef>,
    /// A set of struct names that are determined to be cloneable.
    cloneable_structs: HashSet<String>,
    /// A set of tagged union/error names that are determined to be cloneable.
    cloneable_tagged_unions: HashSet<String>,
    /// A set of all user-defined `error` type names found in the program.
    all_error_types: HashSet<String>,
    /// A flag that is set to `true` if any part of the compiled code uses features
    /// from the C runtime (e.g., `JophetString`, `JophetVector`).
    runtime_needed: bool,
    /// A map from byte offsets to line numbers for the current source file.
    source_map: SourceMap,
    /// A set of C type names that are defined in the runtime and should not be re-generated.
    predefined_runtime_types: HashSet<String>,
    /// Headers to include from `includeC`.
    c_ffi_headers: HashSet<String>,
    /// Flag to include the Python FFI runtime.
    python_runtime_needed: bool,
    /// Flag to indicate if we are building a library.
    is_lib_build: bool,
    /// A buffer for the body of the library's global initializer function.
    library_init_body: String,
}

impl Generator {
    /// Creates a new, empty `Generator`.
    pub fn new(source_map: SourceMap, is_lib_build: bool) -> Self {
        let mut predefined = HashSet::new();
        // Add all Result types from runtime.h
        predefined.insert("Result_Char_Nothing".to_string());
        predefined.insert("Result_void_ptr_void".to_string());
        predefined.insert("Result_int8_t_ParseError".to_string());
        predefined.insert("Result_int16_t_ParseError".to_string());
        predefined.insert("Result_int32_t_ParseError".to_string());
        predefined.insert("Result_int64_t_ParseError".to_string());
        predefined.insert("Result_uint8_t_ParseError".to_string());
        predefined.insert("Result_uint16_t_ParseError".to_string());
        predefined.insert("Result_uint32_t_ParseError".to_string());
        predefined.insert("Result_uint64_t_ParseError".to_string());
        predefined.insert("Result_float_ParseError".to_string());
        predefined.insert("Result_double_ParseError".to_string());
        predefined.insert("Result_JophetString_IoError".to_string());
        predefined.insert("Result_JophetVector_IoError".to_string());
        predefined.insert("Result_void_IoError".to_string());
        predefined.insert("Result_int32_t_CommandError".to_string());
        predefined.insert("Result_PythonModule_FfiError".to_string());
        predefined.insert("Result_int8_t_FfiError".to_string());
        predefined.insert("Result_int16_t_FfiError".to_string());
        predefined.insert("Result_int32_t_FfiError".to_string());
        predefined.insert("Result_int64_t_FfiError".to_string());
        predefined.insert("Result_uint8_t_FfiError".to_string());
        predefined.insert("Result_uint16_t_FfiError".to_string());
        predefined.insert("Result_uint32_t_FfiError".to_string());
        predefined.insert("Result_uint64_t_FfiError".to_string());
        predefined.insert("Result_float_FfiError".to_string());
        predefined.insert("Result_double_FfiError".to_string());
        predefined.insert("Result_bool_FfiError".to_string());
        predefined.insert("Result_char_FfiError".to_string());
        predefined.insert("Result_JophetString_FfiError".to_string());
        predefined.insert("Result_JophetVector_FfiError".to_string());

        // Add the built-in error types themselves to the predefined set.
        predefined.insert("ParseError".to_string());
        predefined.insert("IoError".to_string());
        predefined.insert("CommandError".to_string());
        predefined.insert("FfiError".to_string());
        // Also add the built-in generic "Error" for string messages
        predefined.insert("Error".to_string());

        Generator {
            output: String::new(),
            function_defs: String::new(),
            global_defs: String::new(),
            function_prototypes: IndexSet::new(),
            type_defs: IndexSet::new(),
            vector_clone_helpers: HashMap::new(),
            vector_delete_helpers: HashMap::new(),
            vector_print_helpers: HashMap::new(),
            sprint_helpers: HashSet::new(),
            dictionaries_to_print: HashSet::new(),
            dictionary_delete_thunks: HashMap::new(),
            dictionary_clone_thunks: HashMap::new(),
            python_convertible_types: HashSet::new(),
            array_return_structs: HashSet::new(),
            temp_var_counter: 0,
            scope_cleanup_stack: Vec::new(),
            structs_with_destructors: HashSet::new(),
            tagged_unions_with_destructors: HashSet::new(),
            current_closure_captures: HashSet::new(),
            current_function_return_type: None,
            struct_defs_cache: HashMap::new(),
            enum_defs_cache: HashMap::new(),
            tagged_union_defs_cache: HashMap::new(),
            error_defs_cache: HashMap::new(),
            cloneable_structs: HashSet::new(),
            cloneable_tagged_unions: HashSet::new(),
            all_error_types: HashSet::new(),
            runtime_needed: false,
            source_map,
            predefined_runtime_types: predefined,
            c_ffi_headers: HashSet::new(),
            python_runtime_needed: false,
            is_lib_build,
            library_init_body: String::new(),
        }
    }

    /// Sanitizes a Jophet identifier to avoid conflicts with C keywords.
    fn sanitize_c_keyword(&self, name: &str) -> String {
        match name {
            // List of C keywords that could be valid Jophet identifiers
            "auto" | "break" | "case" | "char" | "const" | "continue" | "default" | "do" |
            "double" | "else" | "enum" | "extern" | "float" | "for" | "goto" | "if" |
            "int" | "long" | "register" | "return" | "short" | "signed" | "sizeof" | "static" |
            "struct" | "switch" | "typedef" | "union" | "unsigned" | "void" | "volatile" | "while" => {
                format!("{}_j", name)
            }
            _ => name.to_string(),
        }
    }

    /// Generates the C code to perform a move for an owned type. This involves
    /// a direct struct copy followed by zeroing out the source to prevent a double-free.
    fn compile_move(&mut self, dest: &str, src: &str, jophet_type: &JophetType) -> String {
        if !self.type_needs_cleanup(jophet_type) {
            return format!("{} = {};", dest, src);
        }
        let c_type = self.jophet_type_to_c_string(jophet_type);
        format!(
            "{{\n\t\t{} = {};\n\t\tmemset(&{}, 0, sizeof({}));\n\t}}",
            dest, src, src, c_type
        )
    }

    /// Formats an optional documentation string into a C Doxygen comment block.
    fn format_doc_comment(&self, comment: &Option<String>) -> String {
        if let Some(text) = comment {
            if text.is_empty() {
                return String::new();
            }
            let mut formatted = String::from("/**\n");
            for line in text.lines() {
                write!(&mut formatted, " * {}\n", line).unwrap();
            }
            formatted.push_str(" */");
            formatted
        } else {
            String::new()
        }
    }

    /// Formats an optional module documentation string into a C Doxygen @file comment block.
    fn format_module_doc_comment(&self, comment: &Option<String>, filename: &str) -> String {
        if let Some(text) = comment {
            if text.is_empty() {
                return String::new();
            }
            let mut formatted = format!("/**\n * @file {}\n", filename);
            for line in text.lines() {
                // Add a blank line for better separation if the user provided one.
                let formatted_line = if line.trim().is_empty() { "" } else { line };
                write!(&mut formatted, " * {}\n", formatted_line).unwrap();
            }
            formatted.push_str(" */");
            formatted
        } else {
            String::new()
        }
    }

    /// The main orchestrator for the code generation process.
    ///
    /// This function takes the entire typed program, runs the generation passes,
    /// and assembles the final C source files into the `BackendOutput` map. It correctly
    /// adds `#include` directives for all imported modules. For library builds with dynamic
    /// globals, it generates an initialization function. For executables, it calls these
    /// initialization functions.
    fn generate_program(
        &mut self,
        program: &TypedProgram,
        public_scope: &ModuleScope,
        module_doc_comment: &Option<String>,
        lib_name: &str,
        imported_modules: &HashMap<String, ModuleScope>,
        filename: &str,
        all_error_defs: &[TypedErrorDef],
        needs_python_runtime: bool,
    ) -> Result<BackendOutput, Box<dyn Error>> {
        self.python_runtime_needed = needs_python_runtime;
        // Pass 1: Generate definitions for all user-defined types and functions.
        self.forward_declare(program, imported_modules, all_error_defs);

        // Pass 2: Compile top-level statements.
        self.scope_cleanup_stack.push(Vec::new());

        for statement in program {
            // For executables, all statements are compiled into the `main` function body.
            // For libraries, top-level variable declarations are handled specially.
            if !matches!(
                statement.kind,
                TypedStatementKind::StructDef(_)
                    | TypedStatementKind::EnumDef(_)
                    | TypedStatementKind::UnionDef(_)
                    | TypedStatementKind::TaggedUnionDef(_)
                    | TypedStatementKind::ErrorDef(_)
                    | TypedStatementKind::FunctionDecl(_)
            ) {
                self.compile_statement_common(statement, true);
            }
        }
        
        // Pass 3: Generate on-demand helper functions now that we know which ones are needed.
        let dicts_to_process: Vec<_> = self.dictionaries_to_print.clone().into_iter().collect();
        for (key_type, value_type) in dicts_to_process {
            self.generate_dictionary_print_function(&key_type, &value_type);
        }

        // Assemble the final C file.
        let mut c_file_content = String::new();

        // Add the file-level doc comment if it exists.
        let file_comment = self.format_module_doc_comment(module_doc_comment, filename);
        writeln!(&mut c_file_content, "{}", file_comment)?;
        writeln!(&mut c_file_content)?;

        // Include runtime header only if needed.
        if self.runtime_needed {
            writeln!(&mut c_file_content, "#include \"runtime.h\"")?;
        }
        if self.python_runtime_needed {
            writeln!(&mut c_file_content, "#include \"jophet_python.h\"")?;
        }
        for header in &self.c_ffi_headers {
            writeln!(&mut c_file_content, "#include <{}>", header)?;
        }

        // Include headers for any imported libraries. This is the key fix.
        for import_name in imported_modules.keys() {
            writeln!(&mut c_file_content, "#include \"{}.h\"", import_name)?;
        }

        // Standard C headers.
        writeln!(&mut c_file_content, "#include <stdio.h>")?;
        writeln!(&mut c_file_content, "#include <stdint.h>")?;
        writeln!(&mut c_file_content, "#include <inttypes.h>")?; // For portable printf format specifiers
        writeln!(&mut c_file_content, "#include <stdbool.h>")?;
        writeln!(&mut c_file_content, "#include <stdlib.h>")?;
        writeln!(&mut c_file_content, "#include <string.h>")?;
        writeln!(&mut c_file_content, "#define _USE_MATH_DEFINES // For M_PI on MSVC")?;
        writeln!(&mut c_file_content, "#include <math.h>")?;
        writeln!(&mut c_file_content)?;

        // Write out all collected type definitions in the order they were generated.
        for def in self.type_defs.iter() {
            writeln!(&mut c_file_content, "{}", def)?;
        }
        writeln!(&mut c_file_content)?;

        // Write out all collected global variable definitions.
        writeln!(&mut c_file_content, "{}", self.global_defs)?;
        writeln!(&mut c_file_content)?;
        
        // Write out all collected function prototypes for functions defined in this file.
        // This now includes prototypes for on-demand helpers.
        for proto in self.function_prototypes.iter() {
            writeln!(&mut c_file_content, "{}", proto)?;
        }
        writeln!(&mut c_file_content)?;

        // Write out all compiled function bodies.
        writeln!(&mut c_file_content, "{}", self.function_defs)?;

        // Write out any generated vector clone helpers.
        if !self.vector_clone_helpers.is_empty() {
            for helper_def in self.vector_clone_helpers.values() {
                writeln!(&mut c_file_content, "{}\n", helper_def)?;
            }
        }

        // Write out any generated vector delete helpers.
        if !self.vector_delete_helpers.is_empty() {
            for helper_def in self.vector_delete_helpers.values() {
                writeln!(&mut c_file_content, "{}\n", helper_def)?;
            }
        }

        // Write out any generated vector print helpers.
        if !self.vector_print_helpers.is_empty() {
            for helper_def in self.vector_print_helpers.values() {
                writeln!(&mut c_file_content, "{}\n", helper_def)?;
            }
        }

        // Write out any generated dictionary delete thunks.
        if !self.dictionary_delete_thunks.is_empty() {
            for thunk_def in self.dictionary_delete_thunks.values() {
                writeln!(&mut c_file_content, "{}\n", thunk_def)?;
            }
        }

        // Write out any generated dictionary clone thunks.
        if !self.dictionary_clone_thunks.is_empty() {
            for thunk_def in self.dictionary_clone_thunks.values() {
                writeln!(&mut c_file_content, "{}\n", thunk_def)?;
            }
        }

        // Generate the FFI dispatcher if needed.
        if self.python_runtime_needed {
            self.generate_python_dispatcher(&mut c_file_content)?;
        }

        // If it's a library build and we have dynamic initializers, generate the init function.
        if self.is_lib_build {
            let init_fn_name = format!("__jophet_init_{}", lib_name.replace('-', "_"));
            writeln!(&mut c_file_content, "void {}() {{", init_fn_name)?;
            // The body can be empty if there are no globals to initialize.
            writeln!(&mut c_file_content, "{}", self.library_init_body)?;
            writeln!(&mut c_file_content, "}}")?;
        }


        // If it's an executable, write the `main` function wrapper.
        if !self.is_lib_build {
            writeln!(&mut c_file_content, "int main(void) {{")?;
            if self.python_runtime_needed {
                writeln!(&mut c_file_content, "\tjophet_py_init();")?;
            }
            // Call the init functions for all imported libraries that have them.
            for import_name in imported_modules.keys() {
                let init_fn_name = format!("__jophet_init_{}", import_name.replace('-', "_"));
                writeln!(&mut c_file_content, "\t{}();", init_fn_name)?;
            }
            writeln!(&mut c_file_content, "{}", self.output)?;
            // Add the final cleanup code for the main scope before returning.
            if let Some(cleanup) = self.scope_cleanup_stack.pop() {
                for stmt in cleanup.iter().rev() {
                    writeln!(&mut c_file_content, "\t{}", stmt)?;
                }
            }
            if self.python_runtime_needed {
                writeln!(&mut c_file_content, "\tjophet_py_finalize();")?;
            }
            writeln!(&mut c_file_content, "\treturn 0;")?;
            writeln!(&mut c_file_content, "}}")?;
        }

        let output_c_filename = Path::new(filename)
            .with_extension("c")
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("output.c")) // Fallback
            .to_string_lossy()
            .into_owned();

        let mut sources = HashMap::new();
        sources.insert(output_c_filename, c_file_content);

        let mut auxiliary_files = HashMap::new();
        // Categorize runtime files correctly: .c is a source, .h is auxiliary.
        if self.runtime_needed {
            let runtime_h = include_str!("runtime.h").to_string();
            let runtime_c = include_str!("runtime.c").to_string();
            auxiliary_files.insert("runtime.h".to_string(), runtime_h);
            sources.insert("runtime.c".to_string(), runtime_c);
        }
        if self.python_runtime_needed {
            let python_h = include_str!("jophet_python.h").to_string();
            let python_c = include_str!("jophet_python.c").to_string();
            auxiliary_files.insert("jophet_python.h".to_string(), python_h);
            sources.insert("jophet_python.c".to_string(), python_c);
        }

        let mut public_headers = HashMap::new();
        if self.is_lib_build {
            let header_content =
                self.generate_library_header(lib_name, public_scope, imported_modules)?;
            public_headers.insert(format!("{}.h", lib_name), header_content);
        }

        let mut output = BackendOutput::new();
        output.insert(OutputFileType::Source, sources);
        output.insert(OutputFileType::Auxiliary, auxiliary_files);
        output.insert(OutputFileType::PublicHeader, public_headers);

        Ok(output)
    }

    /// Generates the `jophet_to_py_object_dispatcher` function.
    /// This function contains a switch statement that calls the correct, generated
    /// helper function to convert a specific aggregate Jophet type to a Python object.
    fn generate_python_dispatcher(&mut self, output: &mut String) -> Result<(), Box<dyn Error>> {
        writeln!(output, "PyObject* jophet_to_py_object_dispatcher(const void* data, JophetTypeTag type) {{")?;
        writeln!(output, "\tswitch(type) {{")?;
        
        // Clone the set to avoid borrowing `self` mutably and immutably at the same time.
        let types_to_process: Vec<_> = self.python_convertible_types.iter().cloned().collect();

        for ty in types_to_process {
            let tag = self.jophet_type_to_c_enum_tag(&ty);
            let helper_name = match &ty {
                JophetType::Tuple(elements) => self.get_or_create_tuple_to_py_tuple_helper(elements),
                JophetType::Struct { name, .. } => self.get_or_create_struct_to_py_dict_helper(name),
                JophetType::Dictionary { key, value } => self.get_or_create_dictionary_to_py_dict_helper(key, value),
                JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } => self.get_or_create_tagged_union_to_py_dict_helper(name),
                _ => continue, // This function only handles aggregate types
            };
            writeln!(output, "\t\tcase {}: return {}(data);", tag, helper_name)?;
        }

        writeln!(output, "\t\tdefault: return NULL; // Should be unreachable")?;
        writeln!(output, "\t}}")?;
        writeln!(output, "}}")?;
        Ok(())
    }

    /// Recursively checks if a type or any of its constituent types
    /// are defined in the C runtime.
    fn type_uses_runtime(&self, ty: &JophetType) -> bool {
        match ty {
            JophetType::String
            | JophetType::Vector(_)
            | JophetType::Dictionary { .. }
            | JophetType::Closure { .. } => true,
            JophetType::Error { name, .. } | JophetType::TaggedUnion { name, .. } => {
                // Check if it's one of the built-in error types from the runtime
                self.predefined_runtime_types.contains(name.as_str())
            }
            JophetType::Fallible { ok, err } => {
                self.type_uses_runtime(ok) || self.type_uses_runtime(err)
            }
            JophetType::Tuple(elements) => elements.iter().any(|t| self.type_uses_runtime(t)),
            JophetType::Array { member_type, .. } => self.type_uses_runtime(member_type),
            JophetType::Pointer(inner)
            | JophetType::Reference(inner)
            | JophetType::MutableReference(inner) => self.type_uses_runtime(inner),
            _ => false,
        }
    }

    /// Generates the content for a public C header file for a Jophet library.
    /// This now includes `extern` declarations for public global variables, prototypes for public
    /// methods, and the necessary `typedef`s for all public types. It also always
    /// includes the library's initialization function prototype.
    fn generate_library_header(
        &mut self,
        lib_name: &str,
        public_scope: &ModuleScope,
        imported_modules: &HashMap<String, ModuleScope>,
    ) -> Result<String, Box<dyn Error>> {
        let mut header = String::new();
        let guard = format!("JOPHET_LIB_{}_H", lib_name.to_uppercase().replace('-', "_"));

        writeln!(&mut header, "#ifndef {}", guard)?;
        writeln!(&mut header, "#define {}", guard)?;
        writeln!(&mut header)?;
        writeln!(&mut header, "// Public API for Jophet library: {}", lib_name)?;
        writeln!(&mut header, "// Generated by the Jophet compiler")?;
        writeln!(&mut header)?;

        writeln!(&mut header, "#include <stdint.h>")?;
        writeln!(&mut header, "#include <stdbool.h>")?;
        writeln!(&mut header, "#include <stddef.h>")?;
        writeln!(&mut header)?;

        let mut written_prototypes = HashSet::new();
        let mut needs_runtime_header = false;

        // A library's public header MUST always declare its init function,
        // even if the function body is empty, so that other modules can link against it.
        let init_fn_name = format!("__jophet_init_{}", lib_name.replace('-', "_"));
        writeln!(&mut header, "void {}();", init_fn_name)?;
        writeln!(&mut header)?;
        
        // --- START OF FIX: Emit public type definitions ---
        writeln!(&mut header, "// Public Type Definitions")?;
        for def in public_scope.struct_defs.values() {
             let c_fields: Vec<String> = def.fields.iter().map(|(name, ty, _)| {
                format!("{} {};", self.jophet_type_to_c_string(ty), self.sanitize_c_keyword(name))
            }).collect();
            writeln!(&mut header, "typedef struct {} {{ {} }} {};", def.name, c_fields.join(" "), def.name)?;
        }
        for def in public_scope.enum_defs.values() {
            let prefixed_members: Vec<String> = def.members.iter().map(|(name, value, _)| {
                format!("{}_{} = {}", def.name, name, value)
            }).collect();
            writeln!(&mut header, "typedef enum {} {{ {} }} {};", def.name, prefixed_members.join(", "), def.name)?;
        }
        // Add similar logic for Union, TaggedUnion, and Error if they can be public
        writeln!(&mut header)?;
        // --- END OF FIX ---


        // Now process all collected functions and variables from the current module's public scope.
        writeln!(&mut header, "// Public Function and Variable Declarations")?;
        for info in public_scope.symbol_table.values() {
            // Check if any type in the signature requires the runtime
            if self.type_uses_runtime(&info.jophet_type) {
                needs_runtime_header = true;
            }

            let proto = match &info.jophet_type {
                JophetType::Function { params, ret } => {
                    let c_return_type = self.jophet_type_to_c_return_string(ret);
                    let mut c_params = Vec::new();
                    for ty in params {
                        let param_type_str = self.jophet_type_to_c_string(ty);
                        let full_param_str = if matches!(ty, JophetType::Array { .. }) {
                            format!("{}* ", self.jophet_type_to_c_string(&self.get_array_base_type(ty)))
                        } else {
                            param_type_str
                        };
                        c_params.push(full_param_str);
                    }

                    let params_str = if c_params.is_empty() {
                        "void".to_string()
                    } else {
                        c_params.join(", ")
                    };

                    format!(
                        "{} {}({});",
                        c_return_type,
                        info.mangled_name.as_ref().unwrap(),
                        params_str
                    )
                }
                // It's a variable, not a function
                _ => {
                    let c_type = self.jophet_type_to_c_string(&info.jophet_type);
                    // Add extern to make it visible to the linker
                    format!(
                        "extern {} {};",
                        c_type,
                        info.mangled_name.as_ref().unwrap()
                    )
                }
            };

            if written_prototypes.insert(proto.clone()) {
                writeln!(&mut header, "{}", proto)?;
            }
        }

        // Process all public methods
        for (type_name, methods) in &public_scope.method_defs {
             for method_info in methods.values() {
                 if self.type_uses_runtime(&method_info.return_type) || method_info.params.iter().any(|(_, ty)| self.type_uses_runtime(ty)) {
                    needs_runtime_header = true;
                }

                let c_return_type = self.jophet_type_to_c_return_string(&method_info.return_type);
                
                let mut c_params: Vec<String> = Vec::new();
                for (i, (name, ty)) in method_info.params.iter().enumerate() {
                    let mut param_type_str = self.jophet_type_to_c_string(ty);
                    let param_name_str = self.sanitize_c_keyword(name);

                    let full_param_str = if matches!(ty, JophetType::Array { .. }) {
                        format!("{} {}{}", self.jophet_type_to_c_string(&self.get_array_base_type(ty)), param_name_str, self.get_array_dimension_suffix(ty))
                    } else {
                        if i == 0 && name == "self" {
                            if !matches!(ty, JophetType::MutableReference(_)) {
                                if let JophetType::Reference(inner) = ty {
                                    if !self.is_primitive_for_clone(inner) {
                                        param_type_str = format!("const {}", param_type_str);
                                    }
                                }
                            }
                        }
                        format!("{} {}", param_type_str, param_name_str)
                    };
                    c_params.push(full_param_str);
                }
                
                let params_str = if c_params.is_empty() { "void".to_string() } else { c_params.join(", ") };
                
                let proto = format!("{} {}({});", c_return_type, method_info.mangled_name, params_str);

                if written_prototypes.insert(proto.clone()) {
                    writeln!(&mut header, "{}", proto)?;
                }
            }
        }


        if needs_runtime_header {
            writeln!(&mut header, "\n// This library uses types from the Jophet runtime.")?;
            writeln!(&mut header, "#include \"runtime.h\"")?;
        }

        writeln!(&mut header)?;
        writeln!(&mut header, "#endif // {}", guard)?;

        Ok(header)
    }
}