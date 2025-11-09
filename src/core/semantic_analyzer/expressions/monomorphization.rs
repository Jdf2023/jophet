// src/core/semantic_analyzer/expressions/monomorphization.rs
//! Contains the semantic analysis logic for monomorphization.
//!
//! This module is responsible for taking generic definitions (like functions and structs)
//! and creating concrete, specialized versions of them based on the types they are used
//! with. This process includes substituting generic type parameters, checking trait bounds,
//! generating unique mangled names for each instantiation, and caching the results to
//! avoid redundant work. It has been updated to use the error-collecting paradigm.

use super::{ScopeContext, SemanticAnalyzer};
use crate::core::ast::typed::*;
use crate::core::ast::untyped;
use crate::core::semantic_analyzer::types::jophet_type_to_user_string;
use crate::diagnostics::errors::SemanticError;
use std::collections::HashMap;

impl SemanticAnalyzer<'_> {
    /// The core monomorphization logic for a generic function call. It infers types, checks
    /// trait bounds, substitutes types in the function's AST, analyzes the new concrete
    /// function in a context that inherits global symbols, caches it, and returns a typed call expression.
    pub fn monomorphize_function_call(
        &mut self,
        name: &str,
        generic_args: &[untyped::Type],
        args: &[untyped::Expression],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let generic_func = self
            .generic_functions
            .get(name)
            .cloned()
            .ok_or_else(|| SemanticError::NameError {
                message: format!("Attempted to monomorphize non-generic function '{}'", name),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            })?;

        // Get the base mangled name from the symbol table. This is crucial for consistency.
        let base_mangled_name = ctx
            .symbol_table
            .get(name)
            .and_then(|info| info.mangled_name.as_ref())
            .cloned()
            .ok_or_else(|| SemanticError::InternalError {
                message: format!(
                    "Generic function '{}' has no base mangled name in the symbol table.",
                    name
                ),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            })?;

        let mut substitutions = HashMap::new();

        // Step 1: Populate substitutions from explicit generic arguments first.
        if generic_args.len() > generic_func.generic_params.len() {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Too many generic arguments for function '{}'. Expected at most {}, found {}.",
                    name,
                    generic_func.generic_params.len(),
                    generic_args.len()
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        for (untyped_arg, gen_param) in
            generic_args.iter().zip(generic_func.generic_params.iter())
        {
            let concrete_type = self.resolve_type(untyped_arg, false, None, ctx, span.clone())?;
            substitutions.insert(gen_param.name.clone(), concrete_type);
        }

        // Step 2 is now smarter. We analyze the runtime arguments using the substitutions
        // we just gathered as hints. This resolves the String vs. StringSlice conflict.
        let mut typed_args = Vec::new();
        for (param_def, arg_expr) in generic_func.params.iter().zip(args.iter()) {
            // Get the parameter's type from the generic function signature (e.g., untyped::Type::Simple("T")).
            let untyped_param_type = &param_def.1;

            // Substitute it with our known concrete types (e.g., substitute "T" with `JophetType::String`).
            let substituted_untyped_param =
                self.substitute_untyped_type(untyped_param_type, &substitutions);

            // Resolve this potentially substituted type into a concrete JophetType. This becomes our hint.
            // If substitution happened, this resolves to `JophetType::String`. If not, it might still be generic.
            let expected_concrete_type = self
                .resolve_type(&substituted_untyped_param, false, None, ctx, arg_expr.span.clone())
                .ok();

            // Now, analyze the argument expression (`"hello jophet"`) with the powerful hint.
            let typed_arg =
                self.analyze_expression(arg_expr, ctx, expected_concrete_type.as_ref(), errors);
            if typed_arg.jophet_type == JophetType::ErrorSentinel { return Ok(typed_arg); }
            typed_args.push(typed_arg);
        }

        // Step 3: Now, use inference from the now-typed arguments for any remaining generic parameters.
        for (param, arg) in generic_func.params.iter().zip(typed_args.iter()) {
            self.infer_substitutions(&param.1, &arg.jophet_type, &mut substitutions, span.clone())?;
        }

        // Step 4: Check if all parameters were filled.
        for gen_param in &generic_func.generic_params {
            if !substitutions.contains_key(&gen_param.name) {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Could not infer type for generic parameter `{}` in call to function `{}`",
                        gen_param.name, name
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        let type_names: Vec<String> = generic_func
            .generic_params
            .iter()
            .map(|p| {
                jophet_type_to_user_string(substitutions.get(&p.name).unwrap())
                    .replace('<', "_")
                    .replace('>', "")
                    .replace(", ", "_")
                    .replace("(", "")
                    .replace(")", "")
                    .replace("&", "ref_")
                    .replace("*", "ptr_")
            })
            .collect();

        // Use the base_mangled_name instead of the short name for consistency.
        let mangled_name = format!("{}_{}", base_mangled_name, type_names.join("_"));

        // Retrieve or create the concrete function definition.
        let needs_creation = !self.monomorphized_functions.borrow().contains_key(&mangled_name);

        if needs_creation {
            // Check trait bounds before proceeding with monomorphization.
            for gen_param in &generic_func.generic_params {
                let concrete_type = substitutions.get(&gen_param.name).unwrap();
                for bound in &gen_param.bounds {
                    if let untyped::Type::Simple(trait_name) = bound {
                        let type_name_str = jophet_type_to_user_string(concrete_type);
                        if self.find_trait_impl(&type_name_str, trait_name).is_none() {
                            return Err(self.generate_missing_trait_impl_error(
                                &type_name_str,
                                trait_name,
                                span.clone(),
                            ));
                        }
                    }
                }
            }

            // Create a concrete untyped function declaration by substituting types.
            let mut concrete_func = generic_func.clone();
            concrete_func.params = concrete_func
                .params
                .into_iter()
                .map(|(name, ty)| (name, self.substitute_untyped_type(&ty, &substitutions)))
                .collect();
            concrete_func.return_type = concrete_func
                .return_type
                .map(|rt| self.substitute_untyped_type(&rt, &substitutions));
            concrete_func.generic_params.clear();

            let mut func_analysis_ctx = ctx.clone();
            func_analysis_ctx.substitutions = substitutions.clone();
            let typed_func_stmt = self
                .analyze_function_like_decl(
                    &concrete_func,
                    &mut func_analysis_ctx,
                    None,
                    None,
                    Some(mangled_name.clone()),
                    span.clone(),
                    errors,
                )?
                .unwrap();
            let typed_func = match typed_func_stmt.kind {
                TypedStatementKind::FunctionDecl(decl) => decl,
                _ => unreachable!(),
            };

            self.monomorphized_functions
                .borrow_mut()
                .insert(mangled_name.clone(), typed_func);
        }

        let functions = self.monomorphized_functions.borrow();
        let typed_func = functions.get(&mangled_name).unwrap();
        let concrete_return_type = typed_func.return_type.clone();

        let expected_params: Vec<_> = typed_func.params.iter().map(|(_, ty)| ty.clone()).collect();
        self.check_arguments(name, &mut typed_args, &expected_params, false, span.clone())?;

        Ok(TypedExpression {
            kind: TypedExpressionKind::FunctionCall {
                kind: TypedCallKind::Named(mangled_name),
                args: typed_args,
            },
            jophet_type: concrete_return_type,
            span,
        })
    }

    /// The core monomorphization logic for a generic struct. It creates a concrete,
    /// mangled version of the struct and all its methods.
    pub fn monomorphize_struct(
        &mut self,
        name: &str,
        generic_args: &[untyped::Type],
        span: crate::core::ast::Span,
    ) -> Result<(TypedStructDef, TypedStructDef, Vec<JophetType>), SemanticError> {
        let generic_def =
            self.struct_defs
                .get(name)
                .cloned()
                .ok_or_else(|| SemanticError::NameError {
                    message: format!("No such generic struct '{}'", name),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                })?;

        if generic_def.generic_params.is_empty() {
            let mut typed_fields = Vec::new();
            let temp_ctx = ScopeContext::new();
            for (field_name, field_type, is_public, _) in &generic_def.fields {
                typed_fields.push((
                    field_name.clone(),
                    self.resolve_type(
                        field_type,
                        true,
                        Some(&generic_def.name),
                        &temp_ctx,
                        span.clone(),
                    )?,
                    *is_public,
                ));
            }

            let mut typed_generic_params = Vec::new();
            for p in &generic_def.generic_params {
                typed_generic_params.push(TypedGenericParam {
                    name: p.name.clone(),
                    bounds: p
                        .bounds
                        .iter()
                        .map(|b| self.resolve_type(b, false, Some(name), &temp_ctx, span.clone()))
                        .collect::<Result<_, _>>()?,
                });
            }
            
            let typed_struct_def = TypedStructDef {
                is_public: generic_def.is_public,
                name: generic_def.name.clone(),
                doc_comment: generic_def.doc_comment.clone(),
                generic_params: typed_generic_params,
                fields: typed_fields,
                module_path: generic_def.module_path.clone(),
            };
            return Ok((typed_struct_def.clone(), typed_struct_def, vec![]));
        }

        if generic_def.generic_params.len() != generic_args.len() {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Incorrect number of generic arguments for struct '{}'. Expected {}, found {}",
                    name,
                    generic_def.generic_params.len(),
                    generic_args.len()
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        let temp_ctx = ScopeContext::new();
        let typed_generic_args: Vec<_> = generic_args
            .iter()
            .map(|arg| self.resolve_type(arg, false, None, &temp_ctx, span.clone()))
            .collect::<Result<_, _>>()?;

        let mangled_name = format!(
            "{}_{}",
            name,
            typed_generic_args
                .iter()
                .map(|t| jophet_type_to_user_string(t)
                    .replace('<', "_")
                    .replace('>', "")
                    .replace(", ", "_"))
                .collect::<Vec<_>>()
                .join("_")
        );

        if let Some(cached_def) = self.monomorphized_structs.borrow().get(&mangled_name) {
            return Ok((cached_def.clone(), cached_def.clone(), typed_generic_args));
        }

        let substitutions: HashMap<_, _> = generic_def
            .generic_params
            .iter()
            .map(|p| p.name.clone())
            .zip(typed_generic_args.iter().cloned())
            .collect();

        let mut concrete_def = generic_def.clone();
        concrete_def.name = mangled_name.clone();
        concrete_def.fields = concrete_def
            .fields
            .into_iter()
            .map(|(fname, ftype, fpub, fdoc)| {
                (
                    fname,
                    self.substitute_untyped_type(&ftype, &substitutions),
                    fpub,
                    fdoc,
                )
            })
            .collect();
        concrete_def.generic_params.clear();

        let mut typed_fields = Vec::new();
        let mut mono_ctx = ScopeContext::new();
        mono_ctx.substitutions = substitutions.clone();
        for (field_name, field_type, is_public, _) in &concrete_def.fields {
            typed_fields.push((
                field_name.clone(),
                self.resolve_type(
                    field_type,
                    true,
                    Some(&concrete_def.name),
                    &mono_ctx,
                    span.clone(),
                )?,
                *is_public,
            ));
        }

        let typed_struct_def = TypedStructDef {
            is_public: concrete_def.is_public,
            name: concrete_def.name.clone(),
            doc_comment: concrete_def.doc_comment.clone(),
            generic_params: vec![],
            fields: typed_fields,
            module_path: concrete_def.module_path.clone(),
        };

        // Monomorphize methods
        if let Some(impl_block) = self.inherent_impl_blocks.get(name).cloned() {
            let mut temp_errors = Vec::new(); // Collect errors, though we can't report them well here
            for method in &impl_block.methods {
                let mut concrete_method = method.clone();
                concrete_method.params = concrete_method
                    .params
                    .into_iter()
                    .map(|(pname, ptype)| (pname, self.substitute_untyped_type(&ptype, &substitutions)))
                    .collect();
                concrete_method.return_type = concrete_method
                    .return_type
                    .map(|rt| self.substitute_untyped_type(&rt, &substitutions));

                let mut method_ctx = ScopeContext::new();
                method_ctx.substitutions = substitutions.clone();
                let mangled_method_name = format!("{}_{}", concrete_def.name, concrete_method.name);
                let typed_method_stmt_res = self.analyze_method_decl(
                    &concrete_method,
                    &concrete_def.name,
                    None,
                    &mut method_ctx,
                    Some(mangled_method_name),
                    span.clone(),
                    &mut temp_errors,
                );
                
                if let Ok(typed_method_stmt) = typed_method_stmt_res {
                    let (mangled_name_key, function_decl) =
                    if let TypedStatementKind::FunctionDecl(d) = typed_method_stmt.kind {
                        (d.mangled_name.clone(), d)
                    } else {
                        unreachable!()
                    };

                    self.monomorphized_functions.borrow_mut()
                        .insert(mangled_name_key, function_decl);
                }
            }
        }

        Ok((
            typed_struct_def.clone(),
            typed_struct_def,
            typed_generic_args,
        ))
    }

    /// Helper to recursively infer generic substitutions by matching a generic type
    /// pattern against a concrete type.
    pub fn infer_substitutions(
        &self,
        pattern: &untyped::Type,
        value: &JophetType,
        substitutions: &mut HashMap<String, JophetType>,
        span: crate::core::ast::Span,
    ) -> Result<(), SemanticError> {
        match pattern {
            untyped::Type::Simple(name) => {
                // This is a generic parameter if it's not a known concrete type.
                if !self.is_known_type(name) {
                    if let Some(existing) = substitutions.get(name) {
                        if existing != value {
                            return Err(SemanticError::TypeError {
                                message: format!(
                                    "Conflicting types inferred for generic parameter `{}`: found both {} and {}",
                                    name,
                                    jophet_type_to_user_string(existing),
                                    jophet_type_to_user_string(value)
                                ),
                                span,
                                file_path: self.current_module_path.clone(),
                            });
                        }
                    } else {
                        substitutions.insert(name.clone(), value.clone());
                    }
                }
            }
            untyped::Type::Generic(pattern_name, pattern_params) => {
                let (value_name, value_params) = match value {
                    JophetType::Vector(inner) => ("Vector", vec![inner.as_ref().clone()]),
                    JophetType::Tuple(elements) => ("Tuple", elements.clone()),
                    _ => return Ok(()),
                };

                if pattern_name == value_name {
                    for (p_param, v_param) in pattern_params.iter().zip(value_params.iter()) {
                        self.infer_substitutions(p_param, v_param, substitutions, span.clone())?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    // Helper to check if a name refers to a known concrete type.
    fn is_known_type(&self, name: &str) -> bool {
        matches!(
            name,
            "Int8"
                | "Int16"
                | "Int32"
                | "Int64"
                | "UInt8"
                | "UInt16"
                | "UInt32"
                | "UInt64"
                | "Float32"
                | "Float64"
                | "Bool"
                | "Char"
                | "String"
                | "Nothing"
        ) || self.struct_defs.contains_key(name)
            || self.enum_defs.contains_key(name)
            || self.union_defs.contains_key(name)
            || self.tagged_union_defs.contains_key(name)
            || self.error_defs.contains_key(name)
            || self.trait_defs.contains_key(name)
    }
}