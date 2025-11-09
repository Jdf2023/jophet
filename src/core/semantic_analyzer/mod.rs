// src/core/semantic_analyzer/mod.rs
//! The semantic analyzer for the Jophet language.
//!
//! This module orchestrates the entire semantic analysis process, which is the
//! most complex phase of the compiler's frontend. It's responsible for:
//!
//! - **Type Checking:** Ensuring that all expressions and statements adhere to the language's type rules.
//! - **Symbol Resolution:** Resolving all identifiers to their declarations and managing scopes.
//! - **Ownership and Borrowing:** Enforcing a simplified ownership and borrow-checking model.
//! - **Trait and Method Resolution:** Finding and validating method calls on structs and other types.
//! - **Monomorphization:** Creating concrete instances of generic functions and structs.
//! - **Error Collection:** Detecting and collecting all semantic errors in a given source file.
//! - **AST Transformation:** Converting the `untyped::Program` from the parser into a `TypedProgram`.

use crate::backend::{BackendType, TargetInfo};
use crate::core::ast::typed::*;
use crate::core::ast::untyped::{self, DeclarationPattern, Program};
use crate::diagnostics::errors::{JophetError, ParserError, SemanticError};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

mod context;
mod declarations;
mod expressions;
mod modules;
mod statements;
pub mod types;

// Re-export key context types for use within the semantic analyzer modules.
pub use context::{ModuleScope, ScopeContext, SymbolInfo};

/// A map to store trait implementations. The key is the type name (e.g., "Int64"),
/// and the value is another map where the key is the trait name (e.g., "Printable")
/// and the value is the untyped implementation block.
type TraitImpls = HashMap<String, HashMap<String, untyped::ImplementBlock>>;

/// The main state-holding struct for the semantic analysis process.
///
/// It contains tables of all definitions (structs, enums, functions, methods) found
/// in the current compilation unit, as well as the scopes of imported modules.
pub struct SemanticAnalyzer<'a> {
    /// Stores all `struct` definitions found in the current module's source.
    struct_defs: HashMap<String, untyped::StructDef>,
    /// Stores all `enum` definitions.
    enum_defs: HashMap<String, untyped::EnumDef>,
    /// Stores all `union` definitions.
    union_defs: HashMap<String, untyped::UnionDef>,
    /// Stores all `tagged union` definitions.
    tagged_union_defs: HashMap<String, untyped::TaggedUnionDef>,
    /// Stores all `error` definitions.
    error_defs: HashMap<String, untyped::ErrorDef>,
    /// Stores all user-defined `error` type names encountered. Used by the backend.
    all_error_types: HashSet<String>,
    /// Stores all `trait` definitions.
    trait_defs: HashMap<String, untyped::TraitDef>,
    /// Stores all `implement Trait for Type` blocks, organized for easy lookup.
    trait_impls: TraitImpls,
    /// Stores all `implement Type` blocks for inherent methods.
    inherent_impl_blocks: HashMap<String, untyped::ImplementBlock>,
    /// Stores validated, but still generic, function templates.
    generic_functions: HashMap<String, untyped::FunctionDecl>,
    /// Caches monomorphized (concrete) function declarations. Key is the mangled name.
    pub monomorphized_functions: RefCell<HashMap<String, TypedFunctionDecl>>,
    /// Caches monomorphized (concrete) struct definitions. Key is the mangled name.
    pub monomorphized_structs: RefCell<HashMap<String, TypedStructDef>>,
    /// Stores the public scopes of all imported modules.
    modules: HashMap<String, ModuleScope>,
    /// Caches function definitions from imported modules with their bodies, keyed by mangled name.
    /// This is needed for compile-time function execution of imported functions.
    /// Note: Function bodies are not serialized in module metadata, so this is populated during
    /// the current compilation session from the imported modules' typed programs.
    pub imported_functions: HashMap<String, TypedFunctionDecl>,
    /// A set to keep track of already processed `import` statements to prevent cycles and redundant work.
    processed_imports: HashSet<String>,
    /// A set of library names that need to be linked by the backend.
    linked_libs: HashSet<String>,
    /// The root directory of the project being compiled.
    project_root: PathBuf,
    /// The path of the specific module file currently being analyzed.
    current_module_path: PathBuf,
    /// The shared directory where dependency artifacts are stored for this build.
    shared_deps_dir: &'a Path,
    /// Flag indicating if this is a release build, passed to recursive builds.
    is_release_build: bool,
    /// Flag to keep intermediate files, passed to recursive builds.
    keep_intermediate: bool,
    /// The backend in use, passed to recursive builds.
    backend_type: BackendType,
    /// A counter for generating unique names for anonymous closure functions.
    closure_counter: usize,
    /// A cache for analyzed closures to prevent re-analysis and naming collisions.
    closure_cache: HashMap<usize, (TypedFunctionDecl, Vec<TypedCapturedVariable>)>,
    /// A flag to indicate that the Python FFI runtime is needed.
    needs_python_runtime: bool,
    /// The resolved `JophetType` for the built-in `PyAny` brand.
    py_any_brand: JophetType,
    /// Information about the target architecture for compilation.
    target_info: &'a TargetInfo,
}

/// The result of a successful semantic analysis pass.
pub struct AnalysisResult {
    /// The final, fully-typed and semantically validated AST.
    pub typed_program: TypedProgram,
    /// A list of external libraries that the final artifact needs to be linked against.
    pub linked_libs: Vec<String>,
    /// The public API of the analyzed module, used for generating metadata files.
    pub public_scope: ModuleScope,
    /// The documentation for the module itself.
    pub module_doc_comment: Option<String>,
    /// The fully analyzed scopes of all modules imported by the program.
    pub imported_modules: HashMap<String, ModuleScope>,
    /// A complete list of all error definitions (user-defined and built-in) for the backend.
    pub all_error_defs: Vec<TypedErrorDef>,
    /// A flag to indicate that the Python FFI runtime is needed.
    pub needs_python_runtime: bool,
}

/// The public entry point for the semantic analyzer.
///
/// This function initializes a `SemanticAnalyzer` and orchestrates the entire analysis process.
///
/// # Arguments
/// * `program` - The `untyped::Program` AST from the parser.
/// * `project_root` - The root path of the project, used for resolving modules.
/// * `main_module_path` - The path to the entry point file (`main.jophet` or `lib.jophet`).
/// * `shared_deps_dir` - The path to the shared directory for dependency artifacts.
/// * `is_release` - Flag indicating if this is a release build.
/// * `keep_intermediate` - Flag to keep temporary build files.
/// * `backend_type` - The backend to use for code generation.
/// * `is_repl_mode` - A special flag for the REPL to treat imported symbols as global.
/// * `target_info` - Information about the target architecture for compilation.
///
/// # Returns
/// A tuple `(AnalysisResult, Vec<SemanticError>)` containing the typed AST and any
/// errors found. If a catastrophic error occurs (like a module not found), it returns a `JophetError`.
pub fn analyze<'a>(
    program: &Program,
    project_root: PathBuf,
    main_module_path: PathBuf,
    shared_deps_dir: &'a Path,
    is_release: bool,
    keep_intermediate: bool,
    backend_type: BackendType,
    is_repl_mode: bool,
    target_info: &'a TargetInfo,
) -> Result<(AnalysisResult, Vec<SemanticError>), JophetError> {
    let mut analyzer = SemanticAnalyzer {
        struct_defs: HashMap::new(),
        enum_defs: HashMap::new(),
        union_defs: HashMap::new(),
        tagged_union_defs: HashMap::new(),
        error_defs: HashMap::new(),
        all_error_types: HashSet::new(),
        trait_defs: HashMap::new(),
        trait_impls: HashMap::new(),
        inherent_impl_blocks: HashMap::new(),
        generic_functions: HashMap::new(),
        monomorphized_functions: RefCell::new(HashMap::new()),
        monomorphized_structs: RefCell::new(HashMap::new()),
        modules: HashMap::new(),
        imported_functions: HashMap::new(),
        processed_imports: HashSet::new(),
        linked_libs: HashSet::new(),
        project_root,
        current_module_path: main_module_path,
        shared_deps_dir,
        is_release_build: is_release,
        keep_intermediate,
        backend_type,
        closure_counter: 0,
        closure_cache: HashMap::new(),
        needs_python_runtime: false,
        py_any_brand: JophetType::ErrorSentinel, // Placeholder, will be initialized.
        target_info,
    };
    analyzer.setup_built_in_types();
    let mut errors = Vec::new();
    let typed_program = analyzer.analyze_program(program, is_repl_mode, &mut errors)?;

    // If there were analysis errors, we can return early.
    if !errors.is_empty() {
        let dummy_result = AnalysisResult {
            typed_program,
            linked_libs: vec![],
            public_scope: ModuleScope::default(),
            module_doc_comment: None,
            imported_modules: HashMap::new(),
            all_error_defs: vec![],
            needs_python_runtime: false,
        };
        return Ok((dummy_result, errors));
    }

    let public_scope = analyzer.extract_public_scope(&typed_program)?;

    // Collect all typed error definitions for the backend.
    let all_error_defs = analyzer
        .error_defs
        .values()
        .map(|untyped_def| {
            let variants = untyped_def
                .variants
                .iter()
                .map(|v| TypedTaggedUnionVariant {
                    name: v.name.clone(),
                    doc_comment: v.doc_comment.clone(),
                    payload: v.payload.as_ref().and_then(|p| {
                        analyzer
                            .resolve_type(
                                p,
                                true,
                                Some(&untyped_def.name),
                                &ScopeContext::new(), // Use an empty context for built-ins
                                Default::default(),
                            )
                            .ok()
                    }),
                })
                .collect();
            TypedErrorDef {
                is_public: untyped_def.is_public,
                name: untyped_def.name.clone(),
                doc_comment: untyped_def.doc_comment.clone(),
                variants,
                module_path: untyped_def.module_path.clone(),
            }
        })
        .collect();

    let result = AnalysisResult {
        typed_program,
        linked_libs: analyzer.linked_libs.into_iter().collect(),
        public_scope,
        module_doc_comment: program.module_doc_comment.clone(),
        imported_modules: analyzer.modules,
        all_error_defs,
        needs_python_runtime: analyzer.needs_python_runtime,
    };

    Ok((result, errors))
}

impl<'a> SemanticAnalyzer<'a> {
    /// Pre-populates the analyzer with definitions for built-in error types and FFI brands.
    /// This ensures they are globally available in all modules without needing an import.
    fn setup_built_in_types(&mut self) {
        let std_path = PathBuf::from("std"); // A virtual path for built-ins

        // --- FFI Brand Types ---
        let py_any_def = untyped::StructDef {
            is_public: true, name: "PyAny".to_string(), doc_comment: Some("A brand representing any Python object.".to_string()),
            generic_params: vec![], fields: vec![], module_path: std_path.clone(),
        };
        self.struct_defs.insert("PyAny".to_string(), py_any_def);
        self.py_any_brand = JophetType::Struct { name: "PyAny".to_string(), module_path: std_path.clone() };

        // Built-in `FfiError` for Python interop
        let ffi_error_def = untyped::ErrorDef {
            is_public: true,
            name: "FfiError".to_string(),
            doc_comment: Some("An error that occurs during a Foreign Function Interface (FFI) call, typically with Python.".to_string()),
            variants: vec![
                untyped::TaggedUnionVariant { name: "ModuleNotFound".to_string(), doc_comment: Some("The requested Python module could not be found.".to_string()), payload: Some(untyped::Type::Simple("String".to_string())) },
                untyped::TaggedUnionVariant { name: "AttributeNotFound".to_string(), doc_comment: Some("The requested function or attribute was not found on the Python object.".to_string()), payload: Some(untyped::Type::Simple("String".to_string())) },
                untyped::TaggedUnionVariant { name: "ConversionFailed".to_string(), doc_comment: Some("Failed to convert a Python object to the requested Jophet type.".to_string()), payload: Some(untyped::Type::Simple("String".to_string())) },
                untyped::TaggedUnionVariant { name: "PythonException".to_string(), doc_comment: Some("An exception was raised within the Python interpreter during the call.".to_string()), payload: Some(untyped::Type::Simple("String".to_string())) },
            ],
            module_path: std_path.clone(),
        };
        self.error_defs.insert("FfiError".to_string(), ffi_error_def);
        self.all_error_types.insert("FfiError".to_string());

        // Built-in `Error` type for general purpose messages
        let general_error_def = untyped::ErrorDef {
            is_public: true,
            name: "Error".to_string(),
            doc_comment: Some("A general-purpose error containing a message.".to_string()),
            variants: vec![untyped::TaggedUnionVariant {
                name: "Message".to_string(),
                doc_comment: Some("An error variant that carries a descriptive string message.".to_string()),
                payload: Some(untyped::Type::Simple("String".to_string())),
            }],
            module_path: std_path.clone(),
        };
        self.error_defs.insert("Error".to_string(), general_error_def);
        self.all_error_types.insert("Error".to_string());

        // Built-in `ParseError` type
        let parse_error_def = untyped::ErrorDef {
            is_public: true,
            name: "ParseError".to_string(),
            doc_comment: Some("An error that occurs during string parsing.".to_string()),
            variants: vec![
                untyped::TaggedUnionVariant { name: "InvalidFormat".to_string(), doc_comment: None, payload: None },
                untyped::TaggedUnionVariant { name: "OutOfRange".to_string(), doc_comment: None, payload: None },
            ],
            module_path: std_path.clone(),
        };
        self.error_defs.insert("ParseError".to_string(), parse_error_def);
        self.all_error_types.insert("ParseError".to_string());

        // Built-in `IoError` type
        let io_error_def = untyped::ErrorDef {
            is_public: true,
            name: "IoError".to_string(),
            doc_comment: Some("An error that occurs during a file I/O operation.".to_string()),
            variants: vec![
                untyped::TaggedUnionVariant { name: "NotFound".to_string(), doc_comment: None, payload: None },
                untyped::TaggedUnionVariant { name: "AccessDenied".to_string(), doc_comment: None, payload: None },
                untyped::TaggedUnionVariant { name: "ReadFailed".to_string(), doc_comment: None, payload: None },
                untyped::TaggedUnionVariant { name: "WriteFailed".to_string(), doc_comment: None, payload: None },
                untyped::TaggedUnionVariant { name: "Other".to_string(), doc_comment: None, payload: Some(untyped::Type::Simple("String".to_string())) },
            ],
            module_path: std_path.clone(),
        };
        self.error_defs.insert("IoError".to_string(), io_error_def);
        self.all_error_types.insert("IoError".to_string());

        // Built-in `CommandError` type
        let command_error_def = untyped::ErrorDef {
            is_public: true,
            name: "CommandError".to_string(),
            doc_comment: Some("An error that occurs while executing a system command.".to_string()),
            variants: vec![
                untyped::TaggedUnionVariant { name: "Failed".to_string(), doc_comment: None, payload: Some(untyped::Type::Simple("Int32".to_string())) },
                untyped::TaggedUnionVariant { name: "TerminatedAbnormally".to_string(), doc_comment: None, payload: None },
                untyped::TaggedUnionVariant { name: "NotFound".to_string(), doc_comment: None, payload: None },
            ],
            module_path: std_path,
        };
        self.error_defs.insert("CommandError".to_string(), command_error_def);
        self.all_error_types.insert("CommandError".to_string());
    }

    /// The main driver for the analysis of a program.
    ///
    /// It performs the analysis in multiple stages:
    /// 1. **Collection Pass**: Gathers all top-level definitions (structs, enums, impl blocks, functions)
    ///    to populate the analyzer's state, making all types and function templates available globally. This
    ///    pass also handles `import` statements, triggering recursive builds for local dependencies if needed.
    /// 2. **Main Analysis Pass**: Analyzes all statements in order. During this pass,
    ///    calls to generic functions trigger monomorphization, which analyzes the concrete versions of
    ///    those functions and adds them to a list for final code generation.
    /// 3. **Cleanup Pass**: After all statements are analyzed, it injects `AutoDelete` statements for any
    ///    heap-allocated variables declared in the main scope to ensure RAII.
    fn analyze_program(
        &mut self,
        program: &Program,
        is_repl_mode: bool,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedProgram, JophetError> {
        let mut ctx = ScopeContext::new();
        let mut main_scope_vars = HashSet::new();

        // Pass 0: Register built-in functions.
        let std_path = PathBuf::from("std");
        ctx.symbol_table.insert(
            "read".to_string(),
            SymbolInfo {
                jophet_type: JophetType::Function {
                    params: vec![JophetType::String],
                    ret: Box::new(JophetType::Fallible {
                        ok: Box::new(JophetType::String),
                        err: Box::new(JophetType::Error {
                            name: "IoError".to_string(),
                            module_path: std_path.clone(),
                        }),
                    }),
                },
                is_mutable: false,
                is_const: false,
                mangled_name: Some("jophet_read".to_string()),
            },
        );
        ctx.symbol_table.insert(
            "readLines".to_string(),
            SymbolInfo {
                jophet_type: JophetType::Function {
                    params: vec![JophetType::String],
                    ret: Box::new(JophetType::Fallible {
                        ok: Box::new(JophetType::Vector(Box::new(JophetType::String))),
                        err: Box::new(JophetType::Error {
                            name: "IoError".to_string(),
                            module_path: std_path.clone(),
                        }),
                    }),
                },
                is_mutable: false,
                is_const: false,
                mangled_name: Some("jophet_read_lines".to_string()),
            },
        );
        ctx.symbol_table.insert(
            "write".to_string(),
            SymbolInfo {
                jophet_type: JophetType::Function {
                    params: vec![JophetType::String, JophetType::String],
                    ret: Box::new(JophetType::Fallible {
                        ok: Box::new(JophetType::Nothing),
                        err: Box::new(JophetType::Error {
                            name: "IoError".to_string(),
                            module_path: std_path.clone(),
                        }),
                    }),
                },
                is_mutable: false,
                is_const: false,
                mangled_name: Some("jophet_write".to_string()),
            },
        );
        ctx.symbol_table.insert(
            "writeLines".to_string(),
            SymbolInfo {
                jophet_type: JophetType::Function {
                    params: vec![
                        JophetType::String,
                        JophetType::Vector(Box::new(JophetType::String)),
                    ],
                    ret: Box::new(JophetType::Fallible {
                        ok: Box::new(JophetType::Nothing),
                        err: Box::new(JophetType::Error {
                            name: "IoError".to_string(),
                            module_path: std_path.clone(),
                        }),
                    }),
                },
                is_mutable: false,
                is_const: false,
                mangled_name: Some("jophet_write_lines".to_string()),
            },
        );
        ctx.symbol_table.insert(
            "allocate".to_string(),
            SymbolInfo {
                jophet_type: JophetType::Function {
                    params: vec![JophetType::UInt(64)],
                    ret: Box::new(JophetType::RawPointer(Box::new(JophetType::Nothing))),
                },
                is_mutable: false,
                is_const: false,
                mangled_name: Some("jophet_allocate".to_string()),
            },
        );
        ctx.symbol_table.insert(
            "deallocate".to_string(),
            SymbolInfo {
                jophet_type: JophetType::Function {
                    params: vec![JophetType::RawPointer(Box::new(JophetType::Nothing))],
                    ret: Box::new(JophetType::Nothing),
                },
                is_mutable: false,
                is_const: false,
                mangled_name: Some("jophet_deallocate".to_string()),
            },
        );

        // Pass 1: Collect all top-level definitions.
        for statement in &program.statements {
            match &statement.kind {
                untyped::StatementKind::Import { path } => {
                    self.analyze_import(path, &mut ctx, statement.span.clone())?;
                    if path.len() == 1 {
                        let name = &path[0];
                        ctx.symbol_table.insert(
                            name.clone(),
                            SymbolInfo {
                                jophet_type: JophetType::Module { name: name.clone() },
                                is_mutable: false,
                                is_const: false,
                                mangled_name: None,
                            },
                        );
                    }

                    // Special handling for the REPL to simulate a single global scope.
                    if is_repl_mode && path.len() == 1 && path[0] == "repl_lib" {
                        if let Some(module_scope) = self.modules.get("repl_lib") {
                            // Merge functions and variables
                            for (symbol_name, symbol_info) in &module_scope.symbol_table {
                                // Merge all public symbols from the REPL's state library
                                // into the current global scope.
                                ctx.symbol_table.insert(symbol_name.clone(), symbol_info.clone());
                            }
                            // Merge type definitions
                            for (name, def) in &module_scope.struct_defs {
                                self.struct_defs
                                    .insert(name.clone(), self.typed_struct_to_untyped(def));
                            }
                            for (name, def) in &module_scope.enum_defs {
                                self.enum_defs
                                    .insert(name.clone(), self.typed_enum_to_untyped(def));
                            }
                            for (name, def) in &module_scope.union_defs {
                                self.union_defs
                                    .insert(name.clone(), self.typed_union_to_untyped(def));
                            }
                            for (name, def) in &module_scope.tagged_union_defs {
                                self.tagged_union_defs.insert(
                                    name.clone(),
                                    self.typed_tagged_union_to_untyped(def),
                                );
                            }
                            for (name, def) in &module_scope.error_defs {
                                self.error_defs
                                    .insert(name.clone(), self.typed_error_to_untyped(def));
                            }
                        }
                    }
                }
                untyped::StatementKind::StructDef(def) => {
                    self.struct_defs.insert(def.name.clone(), def.clone());
                }
                untyped::StatementKind::EnumDef(def) => {
                    self.enum_defs.insert(def.name.clone(), def.clone());
                }
                untyped::StatementKind::UnionDef(def) => {
                    self.union_defs.insert(def.name.clone(), def.clone());
                }
                untyped::StatementKind::TaggedUnionDef(def) => {
                    self.tagged_union_defs.insert(def.name.clone(), def.clone());
                }
                untyped::StatementKind::ErrorDef(def) => {
                    self.error_defs.insert(def.name.clone(), def.clone());
                    self.all_error_types.insert(def.name.clone());
                }
                untyped::StatementKind::TraitDef(def) => {
                    self.trait_defs.insert(def.name.clone(), def.clone());
                    // Also analyze the method signatures to populate the symbol table with `Trait::method` symbols.
                    for method in &def.methods {
                        if let Err(e) = self.analyze_function_like_decl(
                            method,
                            &mut ctx,
                            Some(&def.name),
                            Some(&def.name),
                            None,
                            statement.span.clone(),
                            errors,
                        ) {
                            errors.push(e);
                        }
                    }
                }
                untyped::StatementKind::FunctionDecl(def) => {
                    // For generic functions, just collect them and add a symbol table entry
                    // with their base mangled name. DO NOT analyze their body yet.
                    if !def.generic_params.is_empty() {
                        self.generic_functions.insert(def.name.clone(), def.clone());
                        // We still need to create a base symbol for it so monomorphization can find it.
                        let mangled_name = format!(
                            "{}_{}",
                            self.current_module_path
                                .file_stem()
                                .unwrap()
                                .to_string_lossy(),
                            def.name
                        );
                        // The type here is a placeholder; it's the generic signature that matters.
                        ctx.symbol_table.insert(
                            def.name.clone(),
                            SymbolInfo {
                                jophet_type: JophetType::Nothing, // This type doesn't matter, it's a template
                                is_mutable: false,
                                is_const: false,
                                mangled_name: Some(mangled_name),
                            },
                        );
                    } else {
                        // For non-generic functions, analyze them fully right away.
                        match self.analyze_function_like_decl(
                            def,
                            &mut ctx,
                            None,
                            None,
                            None,
                            statement.span.clone(),
                            errors,
                        ) {
                            Ok(Some(stmt)) => {
                                if let TypedStatementKind::FunctionDecl(typed_decl) = stmt.kind {
                                    self.monomorphized_functions.borrow_mut()
                                        .insert(typed_decl.mangled_name.clone(), typed_decl);
                                }
                            }
                            Err(e) => errors.push(e),
                            Ok(None) => {} // This can happen for generic functions, which we skip here anyway.
                        }
                    }
                }
                untyped::StatementKind::ImplementBlock(imp) => {
                    if let Some(trait_type) = &imp.trait_type {
                        if let (
                            untyped::Type::Simple(type_name),
                            untyped::Type::Simple(trait_name),
                        ) = (&imp.target_type, trait_type)
                        {
                            self.trait_impls
                                .entry(type_name.clone())
                                .or_default()
                                .insert(trait_name.clone(), imp.clone());
                        }
                    } else {
                        if let untyped::Type::Simple(type_name) = &imp.target_type {
                            self.inherent_impl_blocks
                                .insert(type_name.clone(), imp.clone());
                        }
                    }
                }
                _ => {}
            }
        }

        // Collect items to process to avoid borrow checker errors.
        let inherent_impls_to_process: Vec<_> =
            self.inherent_impl_blocks.clone().into_iter().collect();
        let trait_impls_to_process: Vec<_> = self
            .trait_impls
            .clone()
            .into_iter()
            .flat_map(|(type_name, map)| {
                map.into_iter()
                    .map(move |(trait_name, block)| (type_name.clone(), trait_name, block))
            })
            .collect();

        // Analyze methods within impl blocks now that all types are known.
        for (type_name, impl_block) in &inherent_impls_to_process {
            for method in &impl_block.methods {
                if let Ok(Some(stmt)) = self.analyze_function_like_decl(
                    method,
                    &mut ctx,
                    Some(type_name),
                    None,
                    None,
                    method
                        .body
                        .first()
                        .map_or(Default::default(), |s| s.span.clone()),
                    errors,
                ) {
                    if let TypedStatementKind::FunctionDecl(typed_decl) = stmt.kind {
                        self.monomorphized_functions.borrow_mut()
                            .insert(typed_decl.mangled_name.clone(), typed_decl);
                    }
                }
            }
        }
        for (type_name, trait_name, impl_block) in &trait_impls_to_process {
            for method in &impl_block.methods {
                if let Ok(Some(stmt)) = self.analyze_function_like_decl(
                    method,
                    &mut ctx,
                    Some(type_name),
                    Some(trait_name),
                    None,
                    method
                        .body
                        .first()
                        .map_or(Default::default(), |s| s.span.clone()),
                    errors,
                ) {
                    if let TypedStatementKind::FunctionDecl(typed_decl) = stmt.kind {
                        self.monomorphized_functions.borrow_mut()
                            .insert(typed_decl.mangled_name.clone(), typed_decl);
                    }
                }
            }
        }

        // Pass 2: Analyze all statements in a single pass.
        let mut typed_program = Vec::new();
        for stmt in &program.statements {
            // Track variables declared at this top level for final cleanup.
            if let untyped::StatementKind::VariableDecl(decl) = &stmt.kind {
                match &decl.pattern {
                    DeclarationPattern::Identifier(name, _) => {
                        main_scope_vars.insert(name.clone());
                    }
                    DeclarationPattern::Tuple(targets) => {
                        for target in targets {
                            main_scope_vars.insert(target.var_name.clone());
                        }
                    }
                    DeclarationPattern::Array(targets) => {
                        for target in targets {
                            main_scope_vars.insert(target.var_name.clone());
                        }
                    }
                }
            }

            // When we encounter a generic function declaration during this main pass,
            // we must explicitly skip it. It has already been collected and will only
            // be processed via monomorphization.
            if let untyped::StatementKind::FunctionDecl(decl) = &stmt.kind {
                if !decl.generic_params.is_empty() {
                    continue; // Skip generic function templates
                }
            }

            if let Some(typed_stmt) = self.analyze_statement(stmt, &mut ctx, None, false, errors) {
                match &typed_stmt.kind {
                    // Skip adding generic templates and abstract trait defs to the final program.
                    // They are used for analysis but don't generate code directly.
                    TypedStatementKind::FunctionDecl(f) if !f.generic_params.is_empty() => {}
                    TypedStatementKind::TraitDef(_) => {}
                    // Non-generic functions from this main pass are now correctly handled.
                    TypedStatementKind::FunctionDecl(_) => {}
                    _ => typed_program.push(typed_stmt),
                }
            }
        }

        // Add all monomorphized functions (from generics and impl blocks) to the final AST.
        for func in self.monomorphized_functions.borrow().values() {
            typed_program.push(TypedStatement {
                kind: TypedStatementKind::FunctionDecl(func.clone()),
                span: Default::default(),
            });
        }

        // Add all newly monomorphized structs to the final program.
        for def in self.monomorphized_structs.borrow().values() {
            typed_program.push(TypedStatement {
                kind: TypedStatementKind::StructDef(def.clone()),
                span: Default::default(),
            });
        }

        // Pass 3: Perform automatic cleanup for variables declared in the main scope (RAII).
        for var_name in &main_scope_vars {
            if ctx.ownership_map.contains_key(var_name) {
                // --- FIX START ---
                // Replace the unsafe .unwrap() with a safe `if let`.
                // If a variable's declaration failed, it won't be in the symbol table,
                // and we should not try to generate a cleanup call for it. The declaration
                // error has already been reported.
                if let Some(info) = ctx.symbol_table.get(var_name) {
                    let delete_stmt = TypedStatement {
                        kind: TypedStatementKind::AutoDelete(
                            var_name.clone(),
                            info.jophet_type.clone(),
                        ),
                        span: crate::core::ast::Span::default(),
                    };
                    typed_program.push(delete_stmt);
                    // Signal that this variable has been handled for the leak check.
                    ctx.ownership_map.remove(var_name);
                }
                // --- FIX END ---
            }
        }

        // This final check will now only catch resources that genuinely weren't cleaned up.
        if !ctx.ownership_map.is_empty() {
            let leaked_vars: Vec<_> = ctx.ownership_map.keys().cloned().collect();
            errors.push(SemanticError::MemoryError {
                message: format!(
                "Memory leak detected in main program scope. The resources owned by the following variables are not deleted: {:?}",
                leaked_vars
            ),
                span: Default::default(),
                file_path: self.current_module_path.clone(),
            });
        }

        Ok(typed_program)
    }

    /// A placeholder for a separate compile-time evaluation pass. This logic has been
    /// integrated directly into `analyze_simple_variable_decl` for a more robust,
    /// demand-driven evaluation, so this function is now empty.
    fn evaluate_compile_time_constants(
        &self,
        _program: &mut TypedProgram,
        _errors: &mut Vec<SemanticError>,
    ) {
        // This pass is now empty. The logic has been integrated into `analyze_simple_variable_decl`.
        // The demand-driven evaluation is more robust and handles dependencies correctly.
        // Keeping this function as a placeholder in case a final "cleanup" CTFE pass is needed later.
    }

    /// Analyzes a block of statements within a new lexical scope. It now returns whether
    /// a `yield` statement was encountered within the block.
    ///
    /// # Returns
    /// A tuple containing the vector of typed statements and a boolean that is `true`
    /// if a `yield` statement was found in the block.
    fn analyze_block(
        &mut self,
        block: &[untyped::Statement],
        ctx: &mut ScopeContext,
        return_type: Option<&JophetType>,
        in_loop: bool,
        errors: &mut Vec<SemanticError>,
    ) -> (Vec<TypedStatement>, bool) {
        let mut typed_statements = Vec::new();
        let mut locally_declared_vars = HashSet::new();
        let mut did_yield = false;

        for stmt in block {
            if let untyped::StatementKind::Yield(_) = &stmt.kind {
                did_yield = true;
            }
            if let untyped::StatementKind::VariableDecl(decl) = &stmt.kind {
                match &decl.pattern {
                    DeclarationPattern::Identifier(name, _) => {
                        locally_declared_vars.insert(name.clone());
                    }
                    DeclarationPattern::Tuple(targets) => {
                        for target in targets {
                            locally_declared_vars.insert(target.var_name.clone());
                        }
                    }
                    DeclarationPattern::Array(targets) => {
                        for target in targets {
                            locally_declared_vars.insert(target.var_name.clone());
                        }
                    }
                }
            }
            if let Some(typed_stmt) = self.analyze_statement(stmt, ctx, return_type, in_loop, errors)
            {
                typed_statements.push(typed_stmt);
            }
        }

        for var_name in &locally_declared_vars {
            if ctx.ownership_map.contains_key(var_name) {
                // --- FIX START ---
                // Replace the unsafe .unwrap() with a safe `if let`.
                // If a variable's declaration failed, it won't be in the symbol table,
                // and we should not try to generate a cleanup call for it. The declaration
                // error has already been reported.
                if let Some(info) = ctx.symbol_table.get(var_name) {
                    let delete_stmt = TypedStatement {
                        kind: TypedStatementKind::AutoDelete(
                            var_name.clone(),
                            info.jophet_type.clone(),
                        ),
                        span: crate::core::ast::Span::default(),
                    };
                    typed_statements.push(delete_stmt);
                }
                // --- FIX END ---
            }
        }

        // Clean up locally declared vars from the context after the block.
        let owned_vars_in_scope: HashSet<_> = ctx.ownership_map.keys().cloned().collect();
        for var_name in locally_declared_vars {
            if owned_vars_in_scope.contains(&var_name) {
                ctx.ownership_map.remove(&var_name);
            }
            if let Some(owner_name) = ctx.borrows.remove(&var_name) {
                ctx.release_borrow(&owner_name);
            }
            ctx.symbol_table.remove(&var_name);
            ctx.borrow_states.remove(&var_name);
            ctx.moved_vars.remove(&var_name);
            ctx.deleted_vars.remove(&var_name);
        }

        (typed_statements, did_yield)
    }

    fn find_trait_impl(
        &self,
        type_name: &str,
        trait_name: &str,
    ) -> Option<&untyped::ImplementBlock> {
        self.trait_impls
            .get(type_name)
            .and_then(|impls| impls.get(trait_name))
    }

    fn generate_missing_trait_impl_error(
        &self,
        type_name: &str,
        trait_name: &str,
        span: crate::core::ast::Span,
    ) -> SemanticError {
        let trait_def = match self.trait_defs.get(trait_name) {
            Some(def) => def,
            None => {
                return SemanticError::InternalError {
                    message: format!(
                        "Could not find definition for trait '{}' while generating error.",
                        trait_name
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                }
            }
        };

        let mut help_message = format!(
            "The type '{}' does not implement the required trait '{}'.\n",
            type_name, trait_name
        );
        help_message.push_str("Help: You can make this type compliant by adding an implementation.\n");
        help_message.push_str("      Here is a template you can use:\n\n");
        help_message.push_str(&format!("      implement {} for {}\n", trait_name, type_name));

        for method in &trait_def.methods {
            let params = method
                .params
                .iter()
                .map(|(name, ty)| format!("{}: {}", name, ty))
                .collect::<Vec<_>>()
                .join(", ");
            let return_type_str = method
                .return_type
                .as_ref()
                .map_or(String::new(), |t| format!(": {}", t));
            help_message.push_str(&format!(
                "          public function {}(self, {}){}\n",
                method.name, params, return_type_str
            ));
            help_message.push_str("              # TODO: Implement method body\n");
            help_message.push_str("          end\n");
        }
        help_message.push_str("      end\n");

        SemanticError::TypeError {
            message: help_message,
            span,
            file_path: self.current_module_path.clone(),
        }
    }

    /// Extracts the public API from a fully typed program.
    ///
    /// This function scans the final typed AST and collects all items marked as `public`.
    /// This includes functions, structs, enums, etc. It now also correctly identifies
    /// and collects public top-level variable declarations, which is essential for the REPL
    /// and for libraries that export global constants or variables.
    fn extract_public_scope(&self, program: &TypedProgram) -> Result<ModuleScope, JophetError> {
        let mut scope = ModuleScope::default();

        for stmt in program {
            match &stmt.kind {
                TypedStatementKind::StructDef(def) if def.is_public => {
                    scope.struct_defs.insert(def.name.clone(), def.clone());
                }
                TypedStatementKind::EnumDef(def) if def.is_public => {
                    scope.enum_defs.insert(def.name.clone(), def.clone());
                }
                TypedStatementKind::UnionDef(def) if def.is_public => {
                    scope.union_defs.insert(def.name.clone(), def.clone());
                }
                TypedStatementKind::TaggedUnionDef(def) if def.is_public => {
                    scope
                        .tagged_union_defs
                        .insert(def.name.clone(), def.clone());
                }
                TypedStatementKind::ErrorDef(def) if def.is_public => {
                    scope.error_defs.insert(def.name.clone(), def.clone());
                }
                TypedStatementKind::TraitDef(def) if def.is_public => {
                    scope.trait_defs.insert(def.name.clone(), def.clone());
                }
                TypedStatementKind::FunctionDecl(decl) if decl.is_public => {
                    if let Some(receiver_name) = &decl.receiver_type {
                        let method_info = PublicMethodInfo {
                            name: decl.name.clone(),
                            mangled_name: decl.mangled_name.clone(),
                            params: decl.params.clone(),
                            return_type: decl.return_type.clone(),
                        };
                        scope
                            .method_defs
                            .entry(receiver_name.clone())
                            .or_default()
                            .insert(decl.name.clone(), method_info);
                    } else {
                        let func_type = JophetType::Function {
                            params: decl.params.iter().map(|(_, t)| t.clone()).collect(),
                            ret: Box::new(decl.return_type.clone()),
                        };
                        scope.symbol_table.insert(
                            decl.name.clone(),
                            context::SymbolInfo {
                                jophet_type: func_type,
                                is_mutable: false,
                                is_const: false,
                                mangled_name: Some(decl.mangled_name.clone()),
                            },
                        );
                        // Store the function definition with its body for CTFE of imported functions
                        scope
                            .function_defs
                            .insert(decl.mangled_name.clone(), decl.clone());
                    }
                }
                TypedStatementKind::VariableDecl(decl) => {
                    // For a library, all top-level variables are part of its API.
                    // A mangled name is created to avoid collisions in the C global namespace.
                    let mangled_name = format!("__jophet_global_var_{}", decl.name);
                    scope.symbol_table.insert(
                        decl.name.clone(),
                        context::SymbolInfo {
                            jophet_type: decl.jophet_type.clone(),
                            is_mutable: decl.is_mutable,
                            is_const: decl.is_const,
                            mangled_name: Some(mangled_name),
                        },
                    );
                }
                TypedStatementKind::DestructuringDecl(decl) => {
                    for target in &decl.targets {
                        let mangled_name = format!("__jophet_global_var_{}", target.var_name);
                        scope.symbol_table.insert(
                            target.var_name.clone(),
                            context::SymbolInfo {
                                jophet_type: target.jophet_type.clone(),
                                is_mutable: target.is_mutable,
                                is_const: false,
                                mangled_name: Some(mangled_name),
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(scope)
    }
}