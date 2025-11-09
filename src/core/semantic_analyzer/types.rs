// src/core/semantic_analyzer/types.rs
//! Contains the type resolution and checking logic for the semantic analyzer.
//!
//! This module is responsible for:
//! 1. Resolving `untyped::Type` annotations into concrete `JophetType`s.
//! 2. Handling special type names like `Self`.
//! 3. Checking type compatibility for assignments, function returns, etc. This now includes
//!    a stricter check for error types, allowing only `error`-defined types to be
//!    compatible with the universal `AnyError`.
//! 4. Automatically wrapping values into `Fallible` types where appropriate.
//! 5. Validating assignability (l-value checks) for complex expressions.
//! 6. Substituting generic type parameters with concrete types for monomorphization.
//! 7. Looking up methods for any given type, performing just-in-time analysis if needed.

use super::{ScopeContext, SemanticAnalyzer, SymbolInfo};
use crate::core::ast::typed::{JophetType, TypedCallKind, TypedExpression, TypedExpressionKind, TypedStatementKind};
use crate::core::ast::untyped;
use crate::diagnostics::errors::SemanticError;
use std::collections::HashMap;
use std::path::PathBuf;

/// Recursively converts a JophetType into a user-friendly string representation.
pub fn jophet_type_to_user_string(ty: &JophetType) -> String {
    match ty {
        JophetType::ErrorSentinel => "<type error>".to_string(),
        JophetType::Int(b) => format!("Int{}", b),
        JophetType::UInt(b) => format!("UInt{}", b),
        JophetType::Float(b) => format!("Float{}", b),
        JophetType::Bool => "Bool".to_string(),
        JophetType::Char => "Char".to_string(),
        JophetType::String => "String".to_string(),
        JophetType::StringSlice => "StringSlice".to_string(),
        JophetType::Nothing => "Nothing".to_string(),
        JophetType::USize => "USize".to_string(),
        JophetType::ISize => "ISize".to_string(),
        JophetType::Module { name } => format!("Module({})", name),
        JophetType::Struct { name, .. } => name.clone(),
        JophetType::Enum { name, .. } => name.clone(),
        JophetType::Union { name, .. } => name.clone(),
        JophetType::TaggedUnion { name, .. } => name.clone(),
        JophetType::Error { name, .. } => name.clone(),
        JophetType::AnyError => "any_error".to_string(),
        JophetType::Trait { name, .. } => name.clone(),
        JophetType::GenericParam { name } => name.clone(),
        JophetType::Pointer(inner) => format!("Pointer<{}>", jophet_type_to_user_string(inner)),
        JophetType::Reference(inner) => format!("&{}", jophet_type_to_user_string(inner)),
        JophetType::MutableReference(inner) => {
            format!("&mutable {}", jophet_type_to_user_string(inner))
        }
        JophetType::RawPointer(inner) => format!("raw *{}", jophet_type_to_user_string(inner)),
        JophetType::Tuple(types) => {
            let inner = types
                .iter()
                .map(|t| jophet_type_to_user_string(t))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Tuple<({})>", inner)
        }
        JophetType::Array { member_type, size } => {
            format!("Array<{}, {}>", jophet_type_to_user_string(member_type), size)
        }
        JophetType::Vector(inner) => format!("Vector<{}>", jophet_type_to_user_string(inner)),
        JophetType::Dictionary { key, value } => format!(
            "Dictionary<{}, {}>",
            jophet_type_to_user_string(key),
            jophet_type_to_user_string(value)
        ),
        JophetType::UnsizedArray(inner) => {
            format!("Array<{}>", jophet_type_to_user_string(inner))
        }
        JophetType::Function { params, ret } => {
            let p_str = params
                .iter()
                .map(|t| jophet_type_to_user_string(t))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Function({}) -> {}", p_str, jophet_type_to_user_string(ret))
        }
        JophetType::Closure { params, ret, .. } => {
            let p_str = params
                .iter()
                .map(|t| jophet_type_to_user_string(t))
                .collect::<Vec<_>>()
                .join(", ");
            format!("Closure<({}): {}>", p_str, jophet_type_to_user_string(ret))
        }
        JophetType::Fallible { ok, err } => {
            format!(
                "Result<{}, {}>",
                jophet_type_to_user_string(ok),
                jophet_type_to_user_string(err)
            )
        }
        JophetType::CLibrary { header } => format!("CLibrary({})", header.display()),
        JophetType::PythonModule => "PythonModule".to_string(),
        JophetType::PythonObject { brand } => format!("PythonObject<{}>", jophet_type_to_user_string(brand)),
        JophetType::PythonSlice => "PythonSlice".to_string(),
    }
}

impl SemanticAnalyzer<'_> {
    /// Finds a method for a given type, performing just-in-time analysis and monomorphization if necessary.
    ///
    /// This is the centralized logic for method resolution. It checks, in order:
    /// 1. The symbol table for an already-analyzed method.
    /// 2. If the type is imported, it searches the imported module's public `method_defs`.
    /// 3. If not found, it searches inherent and trait `implement` blocks in the current file for an
    ///    untyped method definition.
    /// 4. If an untyped definition is found, it triggers the analysis of that method, which adds the
    ///    typed version to the `monomorphized_functions` map and the symbol table.
    /// 5. It then returns the newly analyzed symbol information.
    ///
    /// # Arguments
    /// * `object_type` - The `JophetType` of the receiver (e.g., `Int64`, `MyStruct`, `T`).
    /// * `method_name` - The name of the method being called.
    /// * `ctx` - A mutable reference to the current scope.
    /// * `span` - The source span of the method call for error reporting.
    ///
    /// # Returns
    /// A `Result` containing `Some(SymbolInfo)` if a method is found, `None` otherwise.
    pub fn find_method_for_type(
        &mut self,
        object_type: &JophetType,
        method_name: &str,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<Option<SymbolInfo>, SemanticError> {
        let base_type = match object_type {
            JophetType::Pointer(inner)
            | JophetType::Reference(inner)
            | JophetType::MutableReference(inner) => inner.as_ref(),
            _ => object_type,
        };

        let type_name_str = jophet_type_to_user_string(base_type);
        let symbol_key = format!("{}::{}", type_name_str, method_name);

        // Priority 1: Check if the method has already been analyzed and is in the symbol table.
        if let Some(info) = ctx.symbol_table.get(&symbol_key) {
            return Ok(Some(info.clone()));
        }

        // Priority 2: If the type is from another module, look in that module's scope metadata.
        if let JophetType::Struct { module_path, .. } = base_type {
            if *module_path != self.current_module_path {
                if let Some(module_name) = module_path.file_stem().and_then(|s| s.to_str()) {
                    if let Some(module_scope) = self.modules.get(module_name) {
                        if let Some(methods) = module_scope.method_defs.get(&type_name_str) {
                            if let Some(method_info) = methods.get(method_name) {
                                // Found the method in the imported module's metadata.
                                // Construct and return the SymbolInfo for it.
                                let symbol_info = SymbolInfo {
                                    jophet_type: JophetType::Function {
                                        params: method_info.params.iter().map(|(_, ty)| ty.clone()).collect(),
                                        ret: Box::new(method_info.return_type.clone()),
                                    },
                                    is_mutable: false,
                                    is_const: false,
                                    mangled_name: Some(method_info.mangled_name.clone()),
                                };
                                return Ok(Some(symbol_info));
                            }
                        }
                    }
                }
            }
        }

        // Priority 3: Look for an untyped method definition in the current file to analyze "just-in-time".
        // Check inherent impls first.
        if let Some(impl_block) = self.inherent_impl_blocks.get(&type_name_str).cloned() {
            if let Some(method_def) = impl_block.methods.iter().find(|m| m.name == method_name) {
                let mut temp_errors = Vec::new();
                let typed_method_stmt = self.analyze_method_decl(
                    method_def,
                    &type_name_str,
                    None,
                    ctx,
                    None,
                    span.clone(),
                    &mut temp_errors,
                )?;
                if !temp_errors.is_empty() {
                    return Err(temp_errors.remove(0));
                }
                if let TypedStatementKind::FunctionDecl(d) = typed_method_stmt.kind {
                    self.monomorphized_functions
                        .borrow_mut()
                        .insert(d.mangled_name.clone(), d);
                    return Ok(ctx.symbol_table.get(&symbol_key).cloned());
                }
            }
        }

        // Priority 4: Check trait impls in the current file.
        if let Some(impl_map) = self.trait_impls.get(&type_name_str).cloned() {
            for (trait_name, impl_block) in impl_map {
                if let Some(method_def) = impl_block.methods.iter().find(|m| m.name == method_name)
                {
                    let mut temp_errors = Vec::new();
                    let typed_method_stmt = self.analyze_method_decl(
                        method_def,
                        &type_name_str,
                        Some(&trait_name),
                        ctx,
                        None,
                        span.clone(),
                        &mut temp_errors,
                    )?;
                    if !temp_errors.is_empty() {
                        return Err(temp_errors.remove(0));
                    }
                    if let TypedStatementKind::FunctionDecl(d) = typed_method_stmt.kind {
                        self.monomorphized_functions
                            .borrow_mut()
                            .insert(d.mangled_name.clone(), d);
                        return Ok(ctx.symbol_table.get(&symbol_key).cloned());
                    }
                }
            }
        }

        // Priority 5: Check generic parameter bounds if the type is a generic parameter.
        if let JophetType::GenericParam {
            name: generic_param_name,
        } = base_type
        {
            if let Some(bounds) = ctx.generic_context.get(generic_param_name) {
                for bound in bounds {
                    if let JophetType::Trait {
                        name: trait_name, ..
                    } = bound
                    {
                        let trait_symbol_key = format!("{}::{}", trait_name, method_name);
                        if let Some(info) = ctx.symbol_table.get(&trait_symbol_key).cloned() {
                            // This method is defined on a trait, not a concrete type.
                            // Its mangled name needs to be specific to the concrete type that will be substituted for T.
                            let mangled_name =
                                format!("{}_{}_{}", type_name_str, trait_name, method_name);
                            let mut new_info = info;
                            new_info.mangled_name = Some(mangled_name);
                            return Ok(Some(new_info));
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// Resolves an `untyped::Type` from the parser into a concrete `JophetType`.
    ///
    /// This function looks up type names in the current scope (including imported modules)
    /// and constructs the final, fully-qualified `JophetType`. It also handles
    /// special type names like `Self` and generic parameters like `T`.
    /// It now checks the `ScopeContext` for active substitutions during monomorphization.
    ///
    /// # Arguments
    /// * `ty` - The untyped type annotation to resolve.
    /// * `is_field` - A flag indicating if the type is for a struct field.
    /// * `self_type_name` - The name of the current struct/impl block, if any.
    /// * `ctx` - The current scope context, used to look up substitutions.
    /// * `span` - The source span for error reporting.
    pub fn resolve_type(
        &self,
        ty: &untyped::Type,
        is_field: bool,
        self_type_name: Option<&str>,
        ctx: &ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<JophetType, SemanticError> {
        match ty {
            untyped::Type::Simple(name) => {
                // Check for a substitution first during monomorphization.
                if let Some(concrete_type) = ctx.substitutions.get(name) {
                    return Ok(concrete_type.clone());
                }

                if name == "Self" {
                    let self_name = self_type_name.ok_or_else(|| SemanticError::TypeError {
                        message:
                            "'Self' can only be used within an `implement` block or a struct definition."
                                .to_string(),
                        span: span.clone(),
                        file_path: self.current_module_path.clone(),
                    })?;
                    if is_field {
                        return Err(SemanticError::TypeError {
                            message:
                                "Cannot use 'Self' as the type for a field inside a struct definition."
                                    .to_string(),
                            span,
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    return self.resolve_type(
                        &untyped::Type::Simple(self_name.to_string()),
                        false,
                        None,
                        ctx,
                        span,
                    );
                }

                if ctx.generic_context.contains_key(name) {
                    return Ok(JophetType::GenericParam { name: name.clone() });
                }

                match name.as_str() {
                    "Int8" => Ok(JophetType::Int(8)),
                    "Int16" => Ok(JophetType::Int(16)),
                    "Int32" => Ok(JophetType::Int(32)),
                    "Int64" => Ok(JophetType::Int(64)),
                    "UInt8" => Ok(JophetType::UInt(8)),
                    "UInt16" => Ok(JophetType::UInt(16)),
                    "UInt32" => Ok(JophetType::UInt(32)),
                    "UInt64" => Ok(JophetType::UInt(64)),
                    "Float32" => Ok(JophetType::Float(32)),
                    "Float64" => Ok(JophetType::Float(64)),
                    "Bool" => Ok(JophetType::Bool),
                    "Char" => Ok(JophetType::Char),
                    "String" => Ok(JophetType::String),
                    "Nothing" => Ok(JophetType::Nothing),
                    "USize" => Ok(JophetType::USize),
                    "ISize" => Ok(JophetType::ISize),
                    "CLibrary" => Ok(JophetType::CLibrary { header: PathBuf::new() }), // Header will be filled by analyze_new_expr
                    "PythonModule" => Ok(JophetType::PythonModule),
                    "PythonObject" => Ok(JophetType::PythonObject { brand: Box::new(self.py_any_brand.clone()) }),
                    "Dictionary" => Err(SemanticError::TypeError {
                        message: "The 'Dictionary' type requires generic arguments, like 'Dictionary<String, Int64>'.".to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    }),
                    _ => {
                        if self.struct_defs.contains_key(name) {
                            Ok(JophetType::Struct {
                                name: name.clone(),
                                module_path: self.current_module_path.clone(),
                            })
                        } else if self.enum_defs.contains_key(name) {
                            let enum_def = &self.enum_defs[name];
                            let mut typed_members = Vec::new();
                            let mut next_value = 0i64;
                            for (member_name, value_opt, doc) in &enum_def.members {
                                let current_value = match value_opt {
                                    Some(val) => *val,
                                    None => next_value,
                                };
                                typed_members.push((member_name.clone(), current_value, doc.clone()));
                                next_value = current_value + 1;
                            }
                            Ok(JophetType::Enum {
                                name: name.clone(),
                                members: typed_members,
                                module_path: self.current_module_path.clone(),
                            })
                        } else if self.union_defs.contains_key(name) {
                            Ok(JophetType::Union {
                                name: name.clone(),
                                module_path: self.current_module_path.clone(),
                            })
                        } else if self.tagged_union_defs.contains_key(name) {
                            Ok(JophetType::TaggedUnion {
                                name: name.clone(),
                                module_path: self.current_module_path.clone(),
                            })
                        } else if self.error_defs.contains_key(name) {
                            Ok(JophetType::Error {
                                name: name.clone(),
                                module_path: self.current_module_path.clone(),
                            })
                        } else if self.trait_defs.contains_key(name) {
                            Ok(JophetType::Trait {
                                name: name.clone(),
                                module_path: self.current_module_path.clone(),
                            })
                        } else {
                            Err(SemanticError::TypeError {
                                message: format!("Unknown type '{}'", name),
                                span,
                                file_path: self.current_module_path.clone(),
                            })
                        }
                    }
                }
            }
            untyped::Type::Generic(name, params) => match name.as_str() {
                "Tuple" => {
                    let member_types = params
                        .iter()
                        .map(|p| self.resolve_type(p, is_field, self_type_name, ctx, span.clone()))
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(JophetType::Tuple(member_types))
                }
                "Vector" if params.len() == 1 => {
                    let member_type =
                        self.resolve_type(&params[0], is_field, self_type_name, ctx, span)?;
                    Ok(JophetType::Vector(Box::new(member_type)))
                }
                "Dictionary" if params.len() == 2 => {
                    let key_type =
                        self.resolve_type(&params[0], is_field, self_type_name, ctx, span.clone())?;
                    let value_type =
                        self.resolve_type(&params[1], is_field, self_type_name, ctx, span.clone())?;

                    // For now, only allow integer and string keys for simplicity.
                    // This could be expanded later with a `Hashable` trait.
                    if !matches!(key_type, JophetType::Int(_) | JophetType::UInt(_) | JophetType::String) {
                        return Err(SemanticError::TypeError {
                            message: format!("Type '{}' cannot be used as a dictionary key. Only integer types and String are currently supported.", jophet_type_to_user_string(&key_type)),
                            span: span.clone(), // This could be improved to span the key type specifically
                            file_path: self.current_module_path.clone(),
                        });
                    }

                    Ok(JophetType::Dictionary {
                        key: Box::new(key_type),
                        value: Box::new(value_type),
                    })
                }
                "Array" => Err(SemanticError::TypeError {
                    message: "Invalid array type annotation. Use `Array<Type, Size>`.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                }),
                "PythonObject" if params.len() == 1 => {
                    let brand_type = self.resolve_type(&params[0], false, None, ctx, span)?;
                    Ok(JophetType::PythonObject { brand: Box::new(brand_type) })
                },
                _ => Err(SemanticError::TypeError {
                    message: format!("Unknown or invalid generic type '{}'", name),
                    span,
                    file_path: self.current_module_path.clone(),
                }),
            },
            untyped::Type::Array(member_type, size) => {
                if *size <= 0 {
                    return Err(SemanticError::TypeError {
                        message: format!("Array size must be a positive integer, but found {}.", size),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let resolved_member_type =
                    self.resolve_type(member_type, is_field, self_type_name, ctx, span)?;
                Ok(JophetType::Array {
                    member_type: Box::new(resolved_member_type),
                    size: *size as usize,
                })
            }
            untyped::Type::Fallible(inner) => {
                let ok_type = self.resolve_type(inner, is_field, self_type_name, ctx, span)?;

                // The error type for any `?` function is the universal `AnyError`.
                let err_type = JophetType::AnyError;

                Ok(JophetType::Fallible {
                    ok: Box::new(ok_type),
                    err: Box::new(err_type),
                })
            }
            untyped::Type::Reference(inner) => Ok(JophetType::Reference(Box::new(
                self.resolve_type(inner, is_field, self_type_name, ctx, span)?,
            ))),
            untyped::Type::MutableReference(inner) => Ok(JophetType::MutableReference(Box::new(
                self.resolve_type(inner, is_field, self_type_name, ctx, span)?,
            ))),
            untyped::Type::RawPointer(inner) => Ok(JophetType::RawPointer(Box::new(
                self.resolve_type(inner, is_field, self_type_name, ctx, span)?,
            ))),
            untyped::Type::Closure { params, ret } => {
                let resolved_params = params
                    .iter()
                    .map(|p| self.resolve_type(p, is_field, self_type_name, ctx, span.clone()))
                    .collect::<Result<Vec<_>, _>>()?;
                let resolved_ret =
                    self.resolve_type(ret, is_field, self_type_name, ctx, span.clone())?;
                Ok(JophetType::Closure {
                    params: resolved_params,
                    ret: Box::new(resolved_ret),
                    mangled_name: String::new(), // Placeholder, will be filled in during analysis.
                    env_struct_name: String::new(), // Placeholder.
                })
            }
        }
    }

    /// Recursively substitutes generic type parameters in a given type with concrete types.
    /// This is the core of monomorphization for typed AST nodes.
    pub(super) fn substitute_type(
        &self,
        ty: &JophetType,
        substitutions: &HashMap<String, JophetType>,
    ) -> JophetType {
        match ty {
            JophetType::GenericParam { name } => {
                substitutions.get(name).cloned().unwrap_or_else(|| ty.clone())
            }
            JophetType::Pointer(inner) => {
                JophetType::Pointer(Box::new(self.substitute_type(inner, substitutions)))
            }
            JophetType::Reference(inner) => {
                JophetType::Reference(Box::new(self.substitute_type(inner, substitutions)))
            }
            JophetType::MutableReference(inner) => {
                JophetType::MutableReference(Box::new(self.substitute_type(inner, substitutions)))
            }
            JophetType::Vector(inner) => {
                JophetType::Vector(Box::new(self.substitute_type(inner, substitutions)))
            }
            JophetType::Tuple(elements) => {
                let new_elements = elements
                    .iter()
                    .map(|el| self.substitute_type(el, substitutions))
                    .collect();
                JophetType::Tuple(new_elements)
            }
            JophetType::Array { member_type, size } => {
                let new_member_type = self.substitute_type(member_type, substitutions);
                JophetType::Array {
                    member_type: Box::new(new_member_type),
                    size: *size,
                }
            }
            JophetType::UnsizedArray(inner) => {
                JophetType::UnsizedArray(Box::new(self.substitute_type(inner, substitutions)))
            }
            JophetType::Function { params, ret } => {
                let new_params = params
                    .iter()
                    .map(|p| self.substitute_type(p, substitutions))
                    .collect();
                let new_ret = self.substitute_type(ret, substitutions);
                JophetType::Function {
                    params: new_params,
                    ret: Box::new(new_ret),
                }
            }
            JophetType::Fallible { ok, err } => {
                let new_ok = self.substitute_type(ok, substitutions);
                let new_err = self.substitute_type(err, substitutions);
                JophetType::Fallible {
                    ok: Box::new(new_ok),
                    err: Box::new(new_err),
                }
            }
            _ => ty.clone(),
        }
    }

    /// Converts a resolved `JophetType` back into an `untyped::Type` for substitution.
    pub(super) fn jophet_type_to_untyped_type(&self, ty: &JophetType) -> untyped::Type {
        match ty {
            JophetType::Int(8) => untyped::Type::Simple("Int8".to_string()),
            JophetType::Int(16) => untyped::Type::Simple("Int16".to_string()),
            JophetType::Int(32) => untyped::Type::Simple("Int32".to_string()),
            JophetType::Int(64) => untyped::Type::Simple("Int64".to_string()),
            JophetType::UInt(8) => untyped::Type::Simple("UInt8".to_string()),
            JophetType::UInt(16) => untyped::Type::Simple("UInt16".to_string()),
            JophetType::UInt(32) => untyped::Type::Simple("UInt32".to_string()),
            JophetType::UInt(64) => untyped::Type::Simple("UInt64".to_string()),
            JophetType::Float(32) => untyped::Type::Simple("Float32".to_string()),
            JophetType::Float(64) => untyped::Type::Simple("Float64".to_string()),
            JophetType::Bool => untyped::Type::Simple("Bool".to_string()),
            JophetType::Char => untyped::Type::Simple("Char".to_string()),
            JophetType::String => untyped::Type::Simple("String".to_string()),
            JophetType::Nothing => untyped::Type::Simple("Nothing".to_string()),
            JophetType::USize => untyped::Type::Simple("USize".to_string()),
            JophetType::ISize => untyped::Type::Simple("ISize".to_string()),
            JophetType::Struct { name, .. } => untyped::Type::Simple(name.clone()),
            JophetType::Enum { name, .. } => untyped::Type::Simple(name.clone()),
            JophetType::Union { name, .. } => untyped::Type::Simple(name.clone()),
            JophetType::TaggedUnion { name, .. } => untyped::Type::Simple(name.clone()),
            JophetType::Error { name, .. } => untyped::Type::Simple(name.clone()),
            JophetType::Vector(inner) => untyped::Type::Generic(
                "Vector".to_string(),
                vec![self.jophet_type_to_untyped_type(inner)],
            ),
            JophetType::Array { member_type, size } => untyped::Type::Array(
                Box::new(self.jophet_type_to_untyped_type(member_type)),
                *size as i64,
            ),
            JophetType::Tuple(elements) => untyped::Type::Generic(
                "Tuple".to_string(),
                elements
                    .iter()
                    .map(|t| self.jophet_type_to_untyped_type(t))
                    .collect(),
            ),
            _ => untyped::Type::Simple("UnsupportedTypeForSubstitution".to_string()),
        }
    }

    /// Converts a typed struct definition back to an untyped one.
    pub(super) fn typed_struct_to_untyped(&self, def: &crate::core::ast::typed::TypedStructDef) -> untyped::StructDef {
        untyped::StructDef {
            is_public: def.is_public,
            name: def.name.clone(),
            doc_comment: def.doc_comment.clone(),
            generic_params: def.generic_params.iter().map(|p| untyped::GenericParam {
                name: p.name.clone(),
                bounds: p.bounds.iter().map(|b| self.jophet_type_to_untyped_type(b)).collect(),
            }).collect(),
            fields: def.fields.iter().map(|(name, ty, is_public)| {
                (name.clone(), self.jophet_type_to_untyped_type(ty), *is_public, None)
            }).collect(),
            module_path: def.module_path.clone(),
        }
    }
    
    /// Converts a typed enum definition back to an untyped one.
    pub(super) fn typed_enum_to_untyped(&self, def: &crate::core::ast::typed::TypedEnumDef) -> untyped::EnumDef {
        untyped::EnumDef {
            is_public: def.is_public,
            name: def.name.clone(),
            doc_comment: def.doc_comment.clone(),
            members: def.members.iter().map(|(name, val, doc)| (name.clone(), Some(*val), doc.clone())).collect(),
            module_path: def.module_path.clone(),
        }
    }

    /// Converts a typed union definition back to an untyped one.
    pub(super) fn typed_union_to_untyped(&self, def: &crate::core::ast::typed::TypedUnionDef) -> untyped::UnionDef {
        untyped::UnionDef {
            is_public: def.is_public,
            name: def.name.clone(),
            doc_comment: def.doc_comment.clone(),
            fields: def.fields.iter().map(|(name, ty, doc)| {
                (name.clone(), self.jophet_type_to_untyped_type(ty), doc.clone())
            }).collect(),
            module_path: def.module_path.clone(),
        }
    }

    /// Converts a typed tagged union definition back to an untyped one.
    pub(super) fn typed_tagged_union_to_untyped(&self, def: &crate::core::ast::typed::TypedTaggedUnionDef) -> untyped::TaggedUnionDef {
        untyped::TaggedUnionDef {
            is_public: def.is_public,
            name: def.name.clone(),
            doc_comment: def.doc_comment.clone(),
            generic_params: def.generic_params.iter().map(|p| untyped::GenericParam {
                name: p.name.clone(),
                bounds: p.bounds.iter().map(|b| self.jophet_type_to_untyped_type(b)).collect(),
            }).collect(),
            variants: def.variants.iter().map(|v| untyped::TaggedUnionVariant {
                name: v.name.clone(),
                doc_comment: v.doc_comment.clone(),
                payload: v.payload.as_ref().map(|p| self.jophet_type_to_untyped_type(p)),
            }).collect(),
            module_path: def.module_path.clone(),
        }
    }
    
    /// Converts a typed error definition back to an untyped one.
    pub(super) fn typed_error_to_untyped(&self, def: &crate::core::ast::typed::TypedErrorDef) -> untyped::ErrorDef {
        untyped::ErrorDef {
            is_public: def.is_public,
            name: def.name.clone(),
            doc_comment: def.doc_comment.clone(),
            variants: def.variants.iter().map(|v| untyped::TaggedUnionVariant {
                name: v.name.clone(),
                doc_comment: v.doc_comment.clone(),
                payload: v.payload.as_ref().map(|p| self.jophet_type_to_untyped_type(p)),
            }).collect(),
            module_path: def.module_path.clone(),
        }
    }


    /// Recursively substitutes generic type parameters in an `untyped::Type`.
    pub(super) fn substitute_untyped_type(
        &self,
        ty: &untyped::Type,
        substitutions: &HashMap<String, JophetType>,
    ) -> untyped::Type {
        match ty {
            untyped::Type::Simple(name) => {
                if let Some(concrete_type) = substitutions.get(name) {
                    self.jophet_type_to_untyped_type(concrete_type)
                } else {
                    ty.clone()
                }
            }
            untyped::Type::Generic(name, params) => {
                let new_params = params
                    .iter()
                    .map(|p| self.substitute_untyped_type(p, substitutions))
                    .collect();
                untyped::Type::Generic(name.clone(), new_params)
            }
            untyped::Type::Array(member_type, size) => {
                let new_member = self.substitute_untyped_type(member_type, substitutions);
                untyped::Type::Array(Box::new(new_member), *size)
            }
            untyped::Type::Reference(inner) => untyped::Type::Reference(Box::new(
                self.substitute_untyped_type(inner, substitutions),
            )),
            untyped::Type::MutableReference(inner) => untyped::Type::MutableReference(Box::new(
                self.substitute_untyped_type(inner, substitutions),
            )),
            untyped::Type::Fallible(inner) => {
                untyped::Type::Fallible(Box::new(self.substitute_untyped_type(inner, substitutions)))
            }
            untyped::Type::Closure { params, ret } => {
                let new_params = params
                    .iter()
                    .map(|p| self.substitute_untyped_type(p, substitutions))
                    .collect();
                let new_ret = self.substitute_untyped_type(ret, substitutions);
                untyped::Type::Closure {
                    params: new_params,
                    ret: Box::new(new_ret),
                }
            }
            untyped::Type::RawPointer(inner) => untyped::Type::RawPointer(Box::new(
                self.substitute_untyped_type(inner, substitutions),
            ))
        }
    }

    /// Determines if a type is a primitive that should be passed by value for `self`.
    pub fn is_primitive_for_self(&self, jophet_type: &JophetType) -> bool {
        matches!(
            jophet_type,
            JophetType::Int(_)
                | JophetType::UInt(_)
                | JophetType::Float(_)
                | JophetType::Bool
                | JophetType::Char
                | JophetType::Enum { .. }
        )
    }

    /// Checks if a type is heap-allocated and requires manual memory management.
    pub fn is_heap_type(&self, ty: &JophetType) -> bool {
        matches!(
            ty,
            JophetType::String | JophetType::Vector(_) | JophetType::Pointer(_) | JophetType::Dictionary { .. } | JophetType::PythonObject { .. }
        )
    }

    /// Recursively determines if a type can be deep-copied.
    ///
    /// A type is cloneable if it's a primitive type, an owned type like `String` or `Vector`
    /// (whose members must also be cloneable), or a struct where all of its fields are also
    /// cloneable. Pointers, references, unions, and modules are not cloneable.
    pub fn is_cloneable(&self, ty: &JophetType) -> bool {
        match ty {
            JophetType::Int(_)
            | JophetType::UInt(_)
            | JophetType::Float(_)
            | JophetType::Bool
            | JophetType::Char
            | JophetType::Enum { .. }
            | JophetType::StringSlice => true,

            JophetType::String | JophetType::Vector(_) | JophetType::Closure { .. } => true,
            JophetType::Dictionary { key, value } => self.is_cloneable(key) && self.is_cloneable(value),

            JophetType::Struct { name, .. } => {
                if let Some(def) = self.struct_defs.get(name) {
                    let temp_ctx = ScopeContext::new(); // Temporary context for resolution
                    def.fields.iter().all(|(_, field_ty, _, _)| {
                        self.resolve_type(field_ty, true, Some(name), &temp_ctx, Default::default())
                            .map_or(false, |resolved_ty| self.is_cloneable(&resolved_ty))
                    })
                } else {
                    false
                }
            }
            JophetType::Array { member_type, .. } => self.is_cloneable(member_type),
            JophetType::Tuple(elements) => elements.iter().all(|t| self.is_cloneable(t)),
            JophetType::GenericParam { .. } => {
                // At this stage, we assume a generic parameter is not cloneable.
                // The check will be enforced by a `T: Cloneable` trait bound at the call site.
                false
            }
            _ => false,
        }
    }

    /// Recursively determines if a type is "printable".
    ///
    /// A type is printable if it's a primitive, a string, a reference to a printable type,
    /// or an aggregate type (struct, vector, tuple, etc.) where all of its members are
    /// also printable. This is used to validate arguments to `println` and `format`.
    pub fn is_printable(&self, ty: &JophetType) -> bool {
        match ty {
            // Primitives are always printable.
            JophetType::Int(_)
            | JophetType::UInt(_)
            | JophetType::Float(_)
            | JophetType::Bool
            | JophetType::Char
            | JophetType::String
            | JophetType::StringSlice
            | JophetType::Enum { .. } => true,

            // Pointers and references are printable if their inner type is.
            JophetType::Pointer(inner)
            | JophetType::Reference(inner)
            | JophetType::MutableReference(inner) => self.is_printable(inner),
            
            // Aggregate types are printable if all their members are.
            JophetType::Vector(inner) => self.is_printable(inner),
            JophetType::Dictionary { key, value } => self.is_printable(key) && self.is_printable(value),
            JophetType::Array { member_type, .. } => self.is_printable(member_type),
            JophetType::Tuple(elements) => elements.iter().all(|el| self.is_printable(el)),

            // Structs, unions, and FFI types have generated or runtime print functions.
            JophetType::Struct { .. }
            | JophetType::Union { .. }
            | JophetType::TaggedUnion { .. }
            | JophetType::Error { .. }
            | JophetType::AnyError
            | JophetType::PythonModule
            | JophetType::PythonObject { .. }
            | JophetType::CLibrary { .. } 
            | JophetType::Closure { .. } => true,
            
            // Other types are not considered printable by default.
            _ => false,
        }
    }

    /// Checks if a `source` type can be safely used where a `target` type is expected.
    /// This is more flexible than `==`. It allows an `Array` to match an `UnsizedArray` hint,
    /// and it now enforces the strict rule that only an `error` type can match `AnyError`.
    pub fn is_type_compatible(&self, source: &JophetType, target: &JophetType) -> bool {
        // If the target is AnyError, only a specific `error` type is compatible for upcasting.
        if let JophetType::AnyError = target {
            if matches!(source, JophetType::Error { .. }) {
                return true;
            }
        }

        match (source, target) {
            (JophetType::Array { member_type: s, .. }, JophetType::UnsizedArray(t)) => s == t,
            (JophetType::UnsizedArray(s), JophetType::Array { member_type: t, .. }) => s == t,
            _ => source == target,
        }
    }

    /// Checks if a type conversion is valid, respecting the `allow_unsafe` flag. This now
    /// also handles unsafe conversions between raw pointers and integers if `allow_unsafe`
    /// (which is controlled by the `allow` keyword) is true.
    ///
    /// # Arguments
    /// * `from` - The source type of the conversion.
    /// * `to` - The target type of the conversion.
    /// * `allow_unsafe` - If `true`, allows conversions that may lose data or are unsafe.
    pub fn is_valid_conversion(
        &self,
        from: &JophetType,
        to: &JophetType,
        allow_unsafe: bool,
    ) -> bool {
        match (from, to) {
            // --- Unsafe Pointer/Integer Conversions (only with `allow`) ---
            (JophetType::RawPointer(_), JophetType::UInt(_)) if allow_unsafe => true,
            (JophetType::UInt(_), JophetType::RawPointer(_)) if allow_unsafe => true,
            (JophetType::RawPointer(_), JophetType::RawPointer(_)) if allow_unsafe => true,
            (JophetType::Array {..}, JophetType::RawPointer(_)) if allow_unsafe => true,

            // --- Safe Numeric Promotions ---
            (JophetType::Int(f), JophetType::Int(t)) if t > f => true,
            (JophetType::UInt(f), JophetType::UInt(t)) if t > f => true,
            (JophetType::Float(32), JophetType::Float(64)) => true,
            (JophetType::Int(_), JophetType::Float(_)) => true,
            (JophetType::UInt(_), JophetType::Float(_)) => true,

            // --- Allowed Numeric Demotions ---
            (JophetType::Int(f), JophetType::Int(t)) if t < f => allow_unsafe,
            (JophetType::UInt(f), JophetType::UInt(t)) if t < f => allow_unsafe,
            (JophetType::Float(64), JophetType::Float(32)) => allow_unsafe,
            (JophetType::Float(_), JophetType::Int(_)) => allow_unsafe,
            (JophetType::Float(_), JophetType::UInt(_)) => allow_unsafe,
            (JophetType::Int(_), JophetType::UInt(_)) => allow_unsafe,
            (JophetType::UInt(_), JophetType::Int(_)) => allow_unsafe,

            // Exact same types are always convertible
            _ if from == to => true,

            // All other conversions are invalid
            _ => false,
        }
    }

    /// A helper function to check argument counts and types for function/method calls.
    ///
    /// It verifies arity and then checks each argument's type against the expected
    /// parameter type, attempting to auto-wrap values into `Fallible` types and
    /// auto-convert `StringSlice` literals to `String` if needed.
    pub fn check_arguments(
        &self,
        callable_name: &str,
        args: &mut Vec<TypedExpression>,
        expected_params: &[JophetType],
        is_method: bool,
        span: crate::core::ast::Span,
    ) -> Result<(), SemanticError> {
        let param_offset = if is_method { 1 } else { 0 };
        if args.len() != expected_params.len() - param_offset {
            return Err(SemanticError::TypeError {
                message: format!(
                    "{} '{}' expects {} arguments, but {} were provided",
                    if is_method { "Method" } else { "Function" },
                    callable_name,
                    expected_params.len() - param_offset,
                    args.len()
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        for (i, (arg, expected_ty)) in args
            .iter_mut()
            .zip(expected_params.iter().skip(param_offset))
            .enumerate()
        {
            if *expected_ty == JophetType::String && arg.jophet_type == JophetType::StringSlice {
                let arg_span = arg.span.clone();
                *arg = TypedExpression {
                    kind: TypedExpressionKind::New {
                        jophet_type: JophetType::String,
                        args: vec![arg.clone()],
                    },
                    jophet_type: JophetType::String,
                    span: arg_span,
                };
            }

            *arg = self.auto_wrap_if_needed(arg.clone(), expected_ty);

            if !self.is_type_compatible(&arg.jophet_type, expected_ty) {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Type mismatch for argument {}. Expected {}, found {}",
                        i + 1,
                        jophet_type_to_user_string(expected_ty),
                        jophet_type_to_user_string(&arg.jophet_type)
                    ),
                    span: arg.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }
        Ok(())
    }

    /// Automatically wraps an expression in a `Fallible` constructor if the context requires it.
    ///
    /// This function now strictly handles error wrapping: it will only wrap a value as the `Err`
    /// variant if that value is of a type defined with the `error` keyword. It no longer
    /// provides syntactic sugar for returning raw strings as errors.
    pub fn auto_wrap_if_needed(
        &self,
        expr: TypedExpression,
        expected_type: &JophetType,
    ) -> TypedExpression {
        if let JophetType::Fallible { ok, err } = expected_type {
            // Case 1: The expression's type matches the expected `ok` type. Wrap it as success.
            if self.is_type_compatible(&expr.jophet_type, ok) {
                return TypedExpression {
                    kind: TypedExpressionKind::FallibleWrap {
                        is_ok: true,
                        expr: Box::new(expr.clone()),
                    },
                    jophet_type: expected_type.clone(),
                    span: expr.span.clone(),
                };
            }

            // Case 2: The expression is a specific error type, and the expected error type
            // is the universal `AnyError`. Wrap it as an error after upcasting.
            if let JophetType::AnyError = err.as_ref() {
                // This logic now ONLY triggers for `JophetType::Error`.
                if matches!(&expr.jophet_type, JophetType::Error { .. }) {
                    // Create an `ErrorUpcast` node to signal the backend to perform the conversion.
                    let upcast_expr = TypedExpression {
                        kind: TypedExpressionKind::ErrorUpcast {
                            expr: Box::new(expr.clone()),
                        },
                        jophet_type: JophetType::AnyError,
                        span: expr.span.clone(),
                    };
                    return TypedExpression {
                        kind: TypedExpressionKind::FallibleWrap {
                            is_ok: false,
                            expr: Box::new(upcast_expr),
                        },
                        jophet_type: expected_type.clone(),
                        span: expr.span.clone(),
                    };
                }
            }
        }
        expr
    }

    /// Attempts to unify two types into a common `Fallible` type if one is a `Fallible`
    /// and the other is one of its variants.
    pub fn try_upgrade_to_fallible(
        &self,
        type1: &JophetType,
        type2: &JophetType,
    ) -> Option<JophetType> {
        if let JophetType::Fallible { ok, err } = type1 {
            if **ok == *type2 || **err == *type2 {
                return Some(type1.clone());
            }
        }
        None
    }

    /// Checks if a given expression is a valid l-value (i.e., can be assigned to).
    /// This is true for mutable variables, fields of mutable structs, elements of mutable
    /// arrays, and dereferences of mutable pointers/references.
    pub fn is_assignable(
        &self,
        expr: &mut TypedExpression,
        ctx: &mut ScopeContext,
    ) -> Result<(bool, String), SemanticError> {
        match &mut expr.kind {
            TypedExpressionKind::Identifier { name, .. } => {
                let info = ctx
                    .symbol_table
                    .get(name)
                    .ok_or_else(|| SemanticError::NameError {
                        message: format!("Cannot find variable '{}' in this scope", name),
                        span: expr.span.clone(),
                        file_path: self.current_module_path.clone(),
                    })?;
                Ok((info.is_mutable, name.clone()))
            }
            TypedExpressionKind::FieldAccess(ref mut object, field) => {
                let (is_obj_assignable, obj_name) = self.is_assignable(object, ctx)?;
                Ok((is_obj_assignable, format!("{}.{}", obj_name, field)))
            }
            TypedExpressionKind::ArrayIndex { ref mut array, .. } => {
                let (is_array_assignable, array_name) = self.is_assignable(array, ctx)?;
                Ok((is_array_assignable, format!("{}[...]", array_name)))
            }
            TypedExpressionKind::Dereference(inner_expr) => {
                let is_mutable_pointer = matches!(
                    inner_expr.jophet_type,
                    JophetType::Pointer(_) | JophetType::MutableReference(_)
                );
                Ok((is_mutable_pointer, "dereferenced value".to_string()))
            }
            _ => Ok((false, "expression".to_string())),
        }
    }

    /// Recursively checks if a type or any of its members requires cleanup.
    pub fn type_needs_cleanup(&self, jophet_type: &JophetType) -> bool {
        match jophet_type {
            JophetType::String | JophetType::Vector(_) | JophetType::Pointer(_) | JophetType::Dictionary { .. } | JophetType::Closure { .. } | JophetType::PythonObject { .. } | JophetType::PythonModule => true,
            JophetType::Struct { name, .. } => {
                if let Some(def) = self.struct_defs.get(name) {
                    let temp_ctx = ScopeContext::new(); // Temporary context for resolution
                    def.fields.iter().any(|(_, field_ty, _, _)| {
                        if let Ok(resolved_ty) =
                            self.resolve_type(field_ty, true, Some(name), &temp_ctx, Default::default())
                        {
                            matches!(
                                resolved_ty,
                                JophetType::String | JophetType::Vector(_) | JophetType::Pointer(_)
                            )
                        } else {
                            false
                        }
                    })
                } else {
                    false
                }
            }
            JophetType::Tuple(types) => types.iter().any(|t| self.type_needs_cleanup(t)),
            JophetType::Array { member_type, .. } => self.type_needs_cleanup(member_type),
            _ => false,
        }
    }

    /// Checks if a type is an "owned" type that is subject to move semantics.
    /// This is true for types that manage heap resources or are aggregates of owned types.
    /// Primitives, references, and simple enums are not owned.
    pub fn is_owned_type(&self, jophet_type: &JophetType) -> bool {
        match jophet_type {
            JophetType::String
            | JophetType::Vector(_)
            | JophetType::Dictionary { .. }
            | JophetType::Struct { .. }
            | JophetType::Tuple(_)
            | JophetType::Array { .. }
            | JophetType::Closure { .. } 
            | JophetType::PythonObject { .. } 
            | JophetType::PythonModule => true,
            _ => false,
        }
    }
}