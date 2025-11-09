// src/core/semantic_analyzer/modules.rs
//! Contains the semantic analysis logic for handling modules and imports.
//!
//! This module is responsible for resolving `import` statements. It can import
//! from several sources, in order of priority:
//! 1. A pre-compiled local dependency from the current build session, by reading
//!    its `.jophet-meta` file from the shared `target/.../deps` directory.
//! 2. A local source module found as a directory within the `src/` directory, with an
//!    entry point at `src/mymodule/lib.jophet`.
//! 3. A local source module found as a single file within the `src/` directory (e.g., `src/mymodule.jophet`).
//! 4. A globally installed library, by reading its `.jophet-meta` file from `~/.jophet`.
//!
//! When a module is imported, its public API is loaded and stored in the main
//! analyzer's context, making it available to the code being compiled. It now
//! correctly recognizes built-in types like `Error` and `ParseError` as valid
//! (though virtual) imports. It also now supports selective imports of functions,
//! types, and methods.

use super::{ModuleScope, ScopeContext, SemanticAnalyzer, SymbolInfo, types::jophet_type_to_user_string};
use crate::core::ast::typed::{JophetType, PublicMethodInfo};
use crate::diagnostics::errors::{JophetError, SemanticError};
use colored::Colorize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

impl SemanticAnalyzer<'_> {
    /// Analyzes an `import` statement, now supporting full, selective, and method imports.
    ///
    /// It handles different import patterns based on the path length:
    /// - `import module`: Makes the module available as a namespace.
    /// - `import module.symbol`: Imports a function, type, or global variable into the current scope.
    /// - `import module.Type.method`: Imports a method as a standalone function.
    ///
    /// This function is now idempotent for selective imports. If a symbol is imported that
    /// already exists in the current scope due to a previous identical import, the operation
    /// is a no-op and does not produce an error.
    ///
    /// # Arguments
    /// * `path` - The dot-separated import path.
    /// * `ctx` - The current scope context, which will be modified with new symbols.
    /// * `span` - The source span of the import statement for error reporting.
    pub fn analyze_import(
        &mut self,
        path: &[String],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<(), JophetError> {
        if path.is_empty() {
            return Err(SemanticError::ModuleError {
                message: "Import path cannot be empty.".to_string(),
                span,
                file_path: self.current_module_path.clone(),
            }
            .into());
        }

        let module_name = &path[0];

        // --- Step 1: Load the root module if it's not already loaded ---
        if !self.processed_imports.contains(module_name) {
            let built_in_types = ["Error", "ParseError", "IoError", "CommandError", "FfiError"];
            if !built_in_types.contains(&module_name.as_str()) {
                let jophet_home = home::home_dir().ok_or_else(|| JophetError::BuildFailed {
                    reason: "Could not find home directory.".to_string(),
                })?;
                let local_meta_path = self.shared_deps_dir.join(format!("{}.jophet-meta", module_name));
                let local_source_dir_path_in_src = self.project_root.join("src").join(module_name);
                let local_source_file_path_in_src = self.project_root.join("src").join(format!("{}.jophet", module_name));
                let installed_meta_path = jophet_home.join(".jophet").join("meta").join(format!("{}.jophet-meta", module_name));

                if local_meta_path.exists() {
                    self.analyze_installed_module(module_name, &local_meta_path, span.clone())?;
                } else if local_source_dir_path_in_src.is_dir() {
                    self.build_and_analyze_local_module(module_name, &local_source_dir_path_in_src, span.clone())?;
                } else if local_source_file_path_in_src.is_file() {
                    self.build_and_analyze_local_module(module_name, &local_source_file_path_in_src, span.clone())?;
                } else if installed_meta_path.exists() {
                    self.analyze_installed_module(module_name, &installed_meta_path, span.clone())?;
                } else {
                    return Err(SemanticError::ModuleError {
                        message: format!("Module '{}' not found. Searched in src/ and installed libraries.", module_name),
                        span,
                        file_path: self.current_module_path.clone(),
                    }.into());
                }
            }
            self.processed_imports.insert(module_name.clone());
        }

        // --- Step 2: Handle the specific import type based on path length ---
        match path.len() {
            1 => {
                // --- Case 1: Full module import (`import my_module`) ---
                ctx.symbol_table.insert(
                    module_name.clone(),
                    SymbolInfo {
                        jophet_type: JophetType::Module { name: module_name.clone() },
                        is_mutable: false,
                        is_const: false,
                        mangled_name: None,
                    },
                );
            }
            2 => {
                // --- Case 2: Selective symbol import (`import my_module.symbol`) ---
                let symbol_name = &path[1];
                let module_scope = self.modules.get(module_name).ok_or_else(|| JophetError::from(SemanticError::InternalError {
                    message: format!("Module '{}' was processed but not found in analyzer state.", module_name),
                    span: span.clone(), file_path: self.current_module_path.clone(),
                }))?;
                
                // Check if this symbol already exists from a local definition or a previous import.
                // If it does, this import is a no-op.
                if ctx.symbol_table.contains_key(symbol_name)
                    || self.struct_defs.contains_key(symbol_name)
                    || self.enum_defs.contains_key(symbol_name)
                    || self.union_defs.contains_key(symbol_name)
                    || self.tagged_union_defs.contains_key(symbol_name)
                    || self.error_defs.contains_key(symbol_name)
                {
                    // The symbol is already in scope. Silently succeed.
                    return Ok(());
                }

                // Look up and import the symbol (function, type, or global var).
                if let Some(info) = module_scope.symbol_table.get(symbol_name) {
                    ctx.symbol_table.insert(symbol_name.clone(), info.clone());
                } else if let Some(def) = module_scope.struct_defs.get(symbol_name) {
                    self.struct_defs.insert(symbol_name.clone(), self.typed_struct_to_untyped(def));
                } else if let Some(def) = module_scope.enum_defs.get(symbol_name) {
                    self.enum_defs.insert(symbol_name.clone(), self.typed_enum_to_untyped(def));
                } else if let Some(def) = module_scope.union_defs.get(symbol_name) {
                    self.union_defs.insert(symbol_name.clone(), self.typed_union_to_untyped(def));
                } else if let Some(def) = module_scope.tagged_union_defs.get(symbol_name) {
                    self.tagged_union_defs.insert(symbol_name.clone(), self.typed_tagged_union_to_untyped(def));
                } else if let Some(def) = module_scope.error_defs.get(symbol_name) {
                    self.error_defs.insert(symbol_name.clone(), self.typed_error_to_untyped(def));
                } else {
                    return Err(JophetError::from(SemanticError::NameError {
                        message: format!("Module '{}' does not have a public member named '{}'.", module_name, symbol_name),
                        span, file_path: self.current_module_path.clone(),
                    }));
                }
            }
            3 => {
                // --- Case 3: Method or member import (`import my_module.MyType.my_method`) ---
                let type_name = &path[1];
                let member_name = &path[2];
                let module_scope = self.modules.get(module_name).ok_or_else(|| JophetError::from(SemanticError::InternalError {
                    message: format!("Module '{}' was processed but not found.", module_name),
                    span: span.clone(), file_path: self.current_module_path.clone(),
                }))?;

                if ctx.symbol_table.contains_key(member_name) {
                    // The method (as a function) is already in scope. Silently succeed.
                    return Ok(());
                }

                // Find the method in the module's public API.
                let method_info = module_scope.method_defs.get(type_name)
                    .and_then(|methods| methods.get(member_name))
                    .ok_or_else(|| JophetError::from(SemanticError::NameError {
                        message: format!("Type '{}' in module '{}' has no public method named '{}'.", type_name, module_name, member_name),
                        span, file_path: self.current_module_path.clone(),
                    }))?;
                
                // Implicitly import the receiver type if it's not already in scope.
                if !self.struct_defs.contains_key(type_name) && !self.tagged_union_defs.contains_key(type_name) {
                    if let Some(def) = module_scope.struct_defs.get(type_name) {
                         self.struct_defs.insert(type_name.clone(), self.typed_struct_to_untyped(def));
                    } else if let Some(def) = module_scope.tagged_union_defs.get(type_name) {
                         self.tagged_union_defs.insert(type_name.clone(), self.typed_tagged_union_to_untyped(def));
                    } else {
                        // Add more type lookups if methods can be on other types.
                    }
                }
                
                // Create a new SymbolInfo representing the method as a free function.
                let function_symbol_info = SymbolInfo {
                    jophet_type: JophetType::Function {
                        // The method's parameters become the function's parameters. 'self' is the first.
                        params: method_info.params.iter().map(|(_, ty)| ty.clone()).collect(),
                        ret: Box::new(method_info.return_type.clone()),
                    },
                    is_mutable: false,
                    is_const: false,
                    mangled_name: Some(method_info.mangled_name.clone()),
                };

                // Add the new function symbol to the current scope.
                ctx.symbol_table.insert(member_name.clone(), function_symbol_info);
            }
            _ => {
                return Err(JophetError::from(SemanticError::ModuleError {
                    message: "Deeper import paths (more than two dots) are not supported.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                }));
            }
        }

        Ok(())
    }

    /// Analyzes an installed library (local or global) by reading its serialized metadata file.
    ///
    /// This function deserializes the `.jophet-meta` JSON file, which contains the
    /// public `ModuleScope` of the library. This scope is then added to the current
    /// analyzer's context, making the library's public types and functions available
    /// for use. It also registers the library name for linking.
    fn analyze_installed_module(
        &mut self,
        name: &str,
        meta_path: &Path,
        span: crate::core::ast::Span,
    ) -> Result<(), JophetError> {
        let meta_content = fs::read_to_string(meta_path).map_err(|e| {
            SemanticError::ModuleError {
                message: format!("Failed to read metadata for module '{}': {}", name, e),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            }
        })?;

        let module_scope: ModuleScope = serde_json::from_str(&meta_content).map_err(|e| {
            SemanticError::ModuleError {
                message: format!("Failed to parse metadata for module '{}': {}", name, e),
                span,
                file_path: self.current_module_path.clone(),
            }
        })?;

        self.modules.insert(name.to_string(), module_scope);
        self.linked_libs.insert(name.to_string());
        Ok(())
    }

    /// Triggers a recursive build for a local module (file or directory) and analyzes its metadata.
    /// It suppresses the "Compiling" message for the special `repl_lib` module to keep the
    /// REPL interface clean.
    fn build_and_analyze_local_module(
        &mut self,
        name: &str,
        module_path: &Path,
        span: crate::core::ast::Span,
    ) -> Result<(), JophetError> {
        // Suppress the "Compiling" message for the internal REPL library to keep the output clean.
        if name != "repl_lib" {
            println!(
                "   {} local module '{}'",
                "Compiling".green().bold(),
                name
            );
        }
        
        let dependency_artifact = crate::commands::build::build_package(
            module_path,
            self.is_release_build,
            false, // is_installing is always false for a local dependency
            self.keep_intermediate,
            self.backend_type,
            self.shared_deps_dir,
            &self.project_root,
            true,  // is_lib: A local module dependency is always a library.
            false, // is_repl_mode: A dependency is never in REPL mode itself
            false, // static_build
            self.target_info.clone(),
        )?;

        let meta_path = dependency_artifact.meta_path.ok_or_else(|| {
            JophetError::from(SemanticError::ModuleError {
                message: format!(
                    "Build of local module '{}' did not produce a metadata file.",
                    name
                ),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            })
        })?;

        self.analyze_installed_module(name, &meta_path, span)?;
        
        // Ensure the library is marked for linking.
        self.linked_libs.insert(name.to_string());
        Ok(())
    }
}