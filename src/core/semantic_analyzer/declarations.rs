// src/core/semantic_analyzer/declarations.rs
//! Contains the semantic analysis logic for declarations.
//!
//! This module implements the analysis for variable declarations (`name: Type = ...` and
//! `mutable name: Type = ...`) and function declarations (`function`). It handles type
//! resolution, symbol table management, and the core ownership and borrowing checks
//! associated with creating new variables and function scopes. It also carries over
//! doc comments from the untyped AST to the typed AST.
//!
//! For `const` declarations, it triggers compile-time evaluation. Crucially, if a variable
//! is initialized with a `const` expression (e.g., `const my_func()`), that variable
//! itself is promoted to a compile-time constant, regardless of whether it was declared with
//! the `const` keyword. This "const-infection" propagates through dependencies, enabling
//! complex compile-time computations.

use super::{types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer};
use crate::core::ast::typed::*;
use crate::core::ast::untyped::{self, DeclarationPattern};
use crate::core::ast::Literal;
use crate::diagnostics::errors::SemanticError;
use std::collections::HashMap;
use std::path::PathBuf;

impl SemanticAnalyzer<'_> {
    /// Helper function to convert an untyped struct definition to a typed one.
    /// This is used for on-the-fly analysis when a non-generic local struct is used.
    fn untyped_struct_to_typed(
        &self,
        def: &untyped::StructDef,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<TypedStructDef, SemanticError> {
        let mut typed_fields = Vec::new();
        for (name, ty, is_public, _) in &def.fields {
            let jophet_type = self.resolve_type(ty, true, Some(&def.name), ctx, span.clone())?;
            typed_fields.push((name.clone(), jophet_type, *is_public));
        }

        let mut typed_generic_params = Vec::new();
        for p in &def.generic_params {
            typed_generic_params.push(TypedGenericParam {
                name: p.name.clone(),
                bounds: p
                    .bounds
                    .iter()
                    .map(|b| self.resolve_type(b, false, Some(&def.name), ctx, span.clone()))
                    .collect::<Result<_, _>>()?,
            });
        }

        Ok(TypedStructDef {
            is_public: def.is_public,
            name: def.name.clone(),
            doc_comment: def.doc_comment.clone(),
            generic_params: typed_generic_params,
            fields: typed_fields,
            module_path: def.module_path.clone(),
        })
    }

    /// Analyzes a variable declaration statement, which can now be a simple identifier
    /// or a destructuring pattern (tuple, struct, or array).
    ///
    /// This function performs several critical checks:
    /// 1. Resolves the declared type annotations from the pattern.
    /// 2. Analyzes the initializer expression to determine its type, using the resolved
    ///    pattern type as a hint for contextual type inference.
    /// 3. Checks for type compatibility between the pattern and the initializer.
    /// 4. Handles ownership transfer (move semantics) and borrowing rules.
    /// 5. Adds all new variables from the pattern to the symbol table, checking for redeclarations.
    /// 6. Specifically handles the `..` rest pattern for all destructuring forms as a discard mechanism.
    pub fn analyze_variable_decl(
        &mut self,
        decl: &untyped::VariableDecl,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        // `const` is currently only supported for simple identifier declarations.
        if decl.is_const {
            if !matches!(decl.pattern, DeclarationPattern::Identifier(_, _)) {
                return Err(SemanticError::TypeError {
                    message: "`const` can only be used with simple identifier declarations (e.g., `const x: T = ...`).".to_string(),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }
        match &decl.pattern {
            DeclarationPattern::Identifier(name, var_type) => {
                self.analyze_simple_variable_decl(name, var_type, decl, decl.is_mutable, decl.is_const, false, ctx, span, errors)
            }
            DeclarationPattern::Tuple(targets) => {
                self.analyze_tuple_destructuring_decl(targets, &decl.initializer, decl.is_mutable, ctx, span, errors)
            }
            DeclarationPattern::Array(targets) => {
                self.analyze_array_destructuring_decl(targets, &decl.initializer, decl.is_mutable, ctx, span, errors)
            }
        }
    }

    /// Analyzes a simple variable declaration (`name: Type = initializer`). It now implements
    /// "const-infection": if the initializer expression is a `const` call, the variable itself
    /// is promoted to a compile-time constant. This value is then computed and stored, making
    /// it available for subsequent compile-time evaluations.
    ///
    /// # Arguments
    /// * `is_comptime_needed` - This flag is used internally by the CTFE engine to signal
    ///   that this variable's value is required for another compile-time evaluation.
    pub fn analyze_simple_variable_decl(
        &mut self,
        name: &str,
        var_type: &untyped::Type,
        decl_node: &untyped::VariableDecl,
        is_mutable: bool,
        is_const: bool,
        is_comptime_needed: bool,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        if is_const && is_mutable {
            return Err(SemanticError::TypeError {
                message: "A variable cannot be declared as both `const` and `mutable`.".to_string(),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        if ctx.symbol_table.contains_key(name) {
            return Err(SemanticError::NameError {
                message: format!("Redeclaration of variable '{}'", name),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        let declared_type = self.resolve_type(var_type, false, None, ctx, span.clone())?;
        if declared_type == JophetType::Nothing {
            return Err(SemanticError::TypeError {
                message: "Cannot declare a variable of type 'Nothing'.".to_string(),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }
        
        // Add the untyped declaration to the map so the interpreter can find it for recursive evaluation.
        ctx.declaration_map.insert(name.to_string(), decl_node.clone());

        // --- START OF FIX ---
        // Determine if the variable should be treated as `const` either because it was
        // explicitly declared as such, or because it is initialized by a `const` call.
        let is_initialized_by_const = matches!(decl_node.initializer.kind, untyped::ExpressionKind::ConstCall { .. });
        let is_effectively_const = is_const || is_initialized_by_const || is_comptime_needed;
        // --- END OF FIX ---

        ctx.current_variable_decl_type = Some(Box::new(declared_type.clone()));
        let mut typed_initializer =
            self.analyze_expression(&decl_node.initializer, ctx, Some(&declared_type), errors);
        ctx.current_variable_decl_type = None;

        if typed_initializer.jophet_type == JophetType::ErrorSentinel {
            return Ok(None);
        }

        // A value must be computed at compile-time if it's effectively constant.
        if is_effectively_const {
            // If the initializer is not already a literal (e.g. `const x = 1 + 2`), evaluate it now.
            if !matches!(typed_initializer.kind, TypedExpressionKind::Literal(_) | TypedExpressionKind::UInt64Literal(_)) {
                 match self.try_evaluate_at_compile_time(&typed_initializer, ctx, errors) {
                    Ok(comptime_value) => {
                        let (new_kind, new_type) = self.comptime_value_to_literal_expr(comptime_value.clone(), typed_initializer.span.clone())?;
                        typed_initializer.kind = new_kind;
                        typed_initializer.jophet_type = new_type;
                        
                        // Add the computed value to our compile-time context for subsequent evaluations.
                        ctx.comptime_values.insert(name.to_string(), comptime_value);
                    },
                    Err(e) => {
                         errors.push(SemanticError::CtfeError {
                            message: format!("Failed to evaluate initializer for '{}' at compile time: {}", name, e),
                            span: decl_node.initializer.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                        return Ok(None);
                    }
                }
            } else if let TypedExpressionKind::Literal(lit) = &typed_initializer.kind {
                // If it's already a literal, convert it to a ComptimeValue and store it.
                let comptime_value = match (lit, &typed_initializer.jophet_type) {
                    (Literal::Int(i), JophetType::UInt(_)) => crate::core::ctfe::ComptimeValue::UInt(*i as u64),
                    (Literal::Int(i), _) => crate::core::ctfe::ComptimeValue::Int(*i),
                    (Literal::Float(f), _) => crate::core::ctfe::ComptimeValue::Float(*f),
                    (Literal::String(s), _) => crate::core::ctfe::ComptimeValue::String(s.clone()),
                    (Literal::Char(c), _) => crate::core::ctfe::ComptimeValue::Char(*c),
                    (Literal::Bool(b), _) => crate::core::ctfe::ComptimeValue::Bool(*b),
                    (Literal::Nothing, _) => crate::core::ctfe::ComptimeValue::Nothing,
                };
                ctx.comptime_values.insert(name.to_string(), comptime_value);
            }
        }
        
        // Core logic for move semantics on initialization.
        if self.is_owned_type(&typed_initializer.jophet_type) {
            if let TypedExpressionKind::Identifier {
                name: source_name, ..
            } = &typed_initializer.kind
            {
                // If the source is being cloned, it's not a move. Otherwise, ownership is transferred.
                if !matches!(&decl_node.initializer.kind, untyped::ExpressionKind::FunctionCall { name, .. } if name == "clone")
                {
                    if ctx.ownership_map.contains_key(source_name) {
                        ctx.moved_vars.insert(source_name.clone());
                    }
                }
            }
        }

        // Ownership and borrow checking.
        if let TypedExpressionKind::Identifier {
            name: source_name, ..
        } = &typed_initializer.kind
        {
            if let Some(alloc_id) = ctx.ownership_map.remove(source_name) {
                ctx.ownership_map.insert(name.to_string(), alloc_id);
            }
        } else if self.is_heap_type(&typed_initializer.jophet_type) {
            let new_id = ctx.new_alloc_id();
            ctx.ownership_map.insert(name.to_string(), new_id);
        }

        if let TypedExpressionKind::AddressOf(borrowed_expr) = &typed_initializer.kind {
            if let TypedExpressionKind::Identifier {
                name: owner_name, ..
            } = &borrowed_expr.kind
            {
                match &typed_initializer.jophet_type {
                    JophetType::Reference(_) => {
                        let owner_state =
                            ctx.borrow_states.get(owner_name).ok_or_else(|| {
                                SemanticError::MemoryError {
                                    message: "Cannot borrow a non-owned value".to_string(),
                                    span: span.clone(),
                                    file_path: self.current_module_path.clone(),
                                }
                            })?;
                        match owner_state {
                            super::context::BorrowState::MutablelyBorrowed => {
                                return Err(SemanticError::MemoryError {
                                    message: format!(
                                    "Cannot borrow `{}` as immutable because it is already borrowed as mutable",
                                    owner_name
                                ),
                                    span: borrowed_expr.span.clone(),
                                    file_path: self.current_module_path.clone(),
                                })
                            }
                            super::context::BorrowState::Unique => ctx.borrow_states.insert(
                                owner_name.clone(),
                                super::context::BorrowState::Borrowed { immutable_count: 1 },
                            ),
                            super::context::BorrowState::Borrowed { immutable_count } => ctx
                                .borrow_states
                                .insert(
                                    owner_name.clone(),
                                    super::context::BorrowState::Borrowed {
                                        immutable_count: immutable_count + 1,
                                    },
                                ),
                        };
                        ctx.borrows.insert(name.to_string(), owner_name.clone());
                    }
                    JophetType::MutableReference(_) => {
                        let owner_info = ctx.symbol_table.get(owner_name).ok_or_else(|| {
                            SemanticError::NameError {
                                message: "Internal error: owner not in symbol table".to_string(),
                                span: span.clone(),
                                file_path: self.current_module_path.clone(),
                            }
                        })?;
                        if !owner_info.is_mutable {
                            return Err(SemanticError::MemoryError {
                                message: format!(
                                "Cannot borrow `{}` as mutable, as it is not declared as mutable",
                                owner_name
                            ),
                                span: borrowed_expr.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                        let owner_state =
                            ctx.borrow_states.get(owner_name).ok_or_else(|| {
                                SemanticError::MemoryError {
                                    message: "Cannot borrow a non-owned value".to_string(),
                                    span: span.clone(),
                                    file_path: self.current_module_path.clone(),
                                }
                            })?;
                        if owner_state != &super::context::BorrowState::Unique {
                            return Err(SemanticError::MemoryError {
                                message: format!(
                                    "Cannot borrow `{}` as mutable because it is already borrowed",
                                    owner_name
                                ),
                                span: borrowed_expr.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                        ctx.borrow_states.insert(
                            owner_name.clone(),
                            super::context::BorrowState::MutablelyBorrowed,
                        );
                        ctx.borrows.insert(name.to_string(), owner_name.clone());
                    }
                    _ => {}
                }
            }
        } else {
            ctx.borrow_states
                .insert(name.to_string(), super::context::BorrowState::Unique);
        }

        let final_type = if let JophetType::Closure { .. } = &typed_initializer.jophet_type {
            // If the initializer is a closure, its type is more specific (contains
            // mangled names). Use it as the variable's final type.
            typed_initializer.jophet_type.clone()
        } else if let JophetType::Struct {
            name: struct_name, ..
        } = &declared_type
        {
            if let JophetType::Pointer(inner) = &typed_initializer.jophet_type {
                if let JophetType::Struct {
                    name: inner_struct_name,
                    ..
                } = inner.as_ref()
                {
                    if struct_name == inner_struct_name {
                        typed_initializer.jophet_type.clone()
                    } else {
                        declared_type.clone()
                    }
                } else {
                    declared_type.clone()
                }
            } else {
                declared_type.clone()
            }
        } else if matches!(declared_type, JophetType::UnsizedArray(_)) {
            typed_initializer.jophet_type.clone()
        } else {
            declared_type.clone()
        };

        if let JophetType::String = &final_type {
            if typed_initializer.jophet_type == JophetType::StringSlice {
                let init_span = typed_initializer.span.clone();
                typed_initializer = TypedExpression {
                    kind: TypedExpressionKind::New {
                        jophet_type: JophetType::String,
                        args: vec![typed_initializer],
                    },
                    jophet_type: JophetType::String,
                    span: init_span,
                };
            }
        }

        if !self.is_type_compatible(&typed_initializer.jophet_type, &final_type) {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Mismatched types for variable '{}'. Expected {}, but initializer has type {}. Did you mean to use `convert({}, {})`?",
                    name,
                    jophet_type_to_user_string(&final_type),
                    jophet_type_to_user_string(&typed_initializer.jophet_type),
                    decl_node.initializer.kind,
                    var_type,
                ),
                span: decl_node.initializer.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        ctx.symbol_table.insert(
            name.to_string(),
            super::context::SymbolInfo {
                jophet_type: typed_initializer.jophet_type.clone(),
                is_mutable,
                is_const: is_effectively_const,
                mangled_name: None,
            },
        );

        let typed_decl = TypedVariableDecl {
            name: name.to_string(),
            is_const: is_effectively_const,
            is_mutable,
            jophet_type: final_type,
            initializer: typed_initializer,
        };

        Ok(Some(TypedStatement {
            kind: TypedStatementKind::VariableDecl(typed_decl),
            span,
        }))
    }

    /// Analyzes a tuple or struct destructuring declaration.
    fn analyze_tuple_destructuring_decl(
        &mut self,
        targets: &[untyped::DestructuringTarget],
        initializer: &untyped::Expression,
        is_statement_mutable: bool,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let typed_initializer = self.analyze_expression(initializer, ctx, None, errors);
        if typed_initializer.jophet_type == JophetType::ErrorSentinel {
            return Ok(None);
        }

        let (initializer_element_types, is_struct) = match &typed_initializer.jophet_type {
            JophetType::Tuple(types) => (types.iter().cloned().collect::<Vec<_>>(), false),
            JophetType::Struct { name, .. } => {
                let struct_def = self
                    .monomorphized_structs.borrow()
                    .get(name)
                    .cloned()
                    .or_else(|| {
                        self.struct_defs.get(name).and_then(|d| {
                            self.untyped_struct_to_typed(d, ctx, span.clone()).ok()
                        })
                    })
                    .or_else(|| {
                        self.modules
                            .values()
                            .find_map(|scope| scope.struct_defs.get(name).cloned())
                    })
                    .ok_or_else(|| SemanticError::NameError {
                        message: format!("Could not find definition for struct '{}'", name),
                        span: initializer.span.clone(),
                        file_path: self.current_module_path.clone(),
                    })?;
                (
                    struct_def
                        .fields
                        .iter()
                        .map(|(_, ftype, _)| ftype.clone())
                        .collect(),
                    true,
                )
            }
            JophetType::Pointer(inner) => {
                if let JophetType::Struct { name, .. } = inner.as_ref() {
                    let struct_def = self
                        .monomorphized_structs.borrow()
                        .get(name)
                        .cloned()
                        .or_else(|| {
                            self.struct_defs.get(name).and_then(|d| {
                                self.untyped_struct_to_typed(d, ctx, span.clone()).ok()
                            })
                        })
                        .or_else(|| {
                            self.modules
                                .values()
                                .find_map(|scope| scope.struct_defs.get(name).cloned())
                        })
                        .ok_or_else(|| SemanticError::NameError {
                            message: format!("Could not find definition for struct '{}'", name),
                            span: initializer.span.clone(),
                            file_path: self.current_module_path.clone(),
                        })?;
                    (
                        struct_def
                            .fields
                            .iter()
                            .map(|(_, ftype, _)| ftype.clone())
                            .collect(),
                        true,
                    )
                } else {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Cannot destructure a value of type '{}'. Only tuples and structs can be destructured.",
                            jophet_type_to_user_string(&typed_initializer.jophet_type)
                        ),
                        span: initializer.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
            }
            _ => {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Cannot destructure a value of type '{}'. Only tuples and structs can be destructured.",
                        jophet_type_to_user_string(&typed_initializer.jophet_type)
                    ),
                    span: initializer.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        };

        let rest_pattern_exists = targets.iter().any(|t| t.is_rest_pattern);
        let num_explicit_targets = targets.len() - if rest_pattern_exists { 1 } else { 0 };
        let is_labeled_pattern = targets.iter().any(|t| t.source_field.is_some());
        let is_actual_named_destructuring =
            is_struct && targets.iter().any(|t| t.source_field.is_some() && t.var_name != "_");
        let is_actual_positional_destructuring = !is_actual_named_destructuring;

        if is_struct
            && is_labeled_pattern
            && targets
                .iter()
                .any(|t| t.source_field.is_none() && t.var_name != "_" && !t.is_rest_pattern)
        {
            return Err(SemanticError::SyntaxError {
                message: "Mixing labeled and positional destructuring is not allowed for structs. If one field is labeled, all non-discard targets must be labeled.".to_string(),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        if num_explicit_targets > initializer_element_types.len() {
            let expected_kind = if is_struct { "fields" } else { "elements" };
            return Err(SemanticError::TypeError {
                message: format!(
                    "Too many explicit variables in destructuring. The pattern has {} variables, but the initializer has only {} {}.",
                    num_explicit_targets,
                    initializer_element_types.len(),
                    expected_kind,
                ),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        if !rest_pattern_exists
            && is_actual_positional_destructuring
            && (targets.len() != initializer_element_types.len())
        {
            let expected_kind = if is_struct { "fields" } else { "elements" };
            let message = format!(
                "Mismatched arity in destructuring declaration. The pattern has {} variables, but the initializer has {} {}. Positional destructuring (for tuples and structs) must be exhaustive without a rest pattern (`..`).",
                targets.len(),
                initializer_element_types.len(),
                expected_kind,
            );
            return Err(SemanticError::TypeError {
                message,
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        if is_labeled_pattern && !is_struct {
            return Err(SemanticError::TypeError {
                message: "Labeled destructuring can only be used with structs.".to_string(),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        let mut typed_targets = Vec::new();

        for (i, target) in targets.iter().enumerate() {
            if target.is_rest_pattern || target.var_name == "_" {
                continue;
            }

            if is_statement_mutable && target.is_mutable {
                return Err(SemanticError::SyntaxError {
                    message: format!(
                        "Redundant `mutable` keyword for variable '{}'. When the entire declaration starts with `mutable`, all variables in the pattern are implicitly mutable. Remove `mutable` before '{}'.",
                        target.var_name, target.var_name
                    ),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            if is_actual_named_destructuring && target.source_field.is_none() {
                return Err(SemanticError::InternalError {
                    message: format!("Internal error: Positional target '{}' found in named destructuring context.", target.var_name),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            let declared_target_type = self.resolve_type(&target.ty, false, None, ctx, span.clone())?;
            let expected_element_type =
                initializer_element_types.get(i).cloned().unwrap_or(JophetType::Nothing);

            if !self.is_type_compatible(&declared_target_type, &expected_element_type) {
                return Err(SemanticError::TypeError {
                   message: format!(
                       "Type mismatch for variable '{}' in destructuring. The pattern expects type {}, but the initializer provides type {}.",
                       target.var_name,
                       jophet_type_to_user_string(&declared_target_type),
                       jophet_type_to_user_string(&expected_element_type)
                   ),
                   span: span.clone(),
                   file_path: self.current_module_path.clone(),
               });
            }

            if ctx.symbol_table.contains_key(&target.var_name) {
                return Err(SemanticError::NameError {
                    message: format!(
                        "Redeclaration of variable '{}' in the same scope.",
                        target.var_name
                    ),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            if let Some(source_field) = &target.source_field {
                let base_struct_name = match &typed_initializer.jophet_type {
                    JophetType::Pointer(inner_type)
                    | JophetType::Reference(inner_type)
                    | JophetType::MutableReference(inner_type) => {
                        if let JophetType::Struct { name, .. } = inner_type.as_ref() {
                            name
                        } else {
                            unreachable!()
                        }
                    }
                    JophetType::Struct { name, .. } => name,
                    _ => unreachable!(),
                };
                let struct_def_untyped = self.struct_defs.get(base_struct_name).unwrap();
                if struct_def_untyped
                    .fields
                    .iter()
                    .find(|(n, _, _, _)| n == source_field)
                    .is_none()
                {
                    return Err(SemanticError::NameError {
                        message: format!(
                            "Struct '{}' has no field named '{}' to destructure from.",
                            base_struct_name, source_field
                        ),
                        span: span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
            }

            let final_target_mutability = is_statement_mutable || target.is_mutable;
            ctx.symbol_table.insert(
                target.var_name.clone(),
                super::context::SymbolInfo {
                    jophet_type: expected_element_type.clone(),
                    is_mutable: final_target_mutability,
                    is_const: false,
                    mangled_name: None,
                },
            );

            typed_targets.push(TypedDestructuringTarget {
                var_name: target.var_name.clone(),
                jophet_type: expected_element_type,
                is_mutable: final_target_mutability,
                source_field: target.source_field.clone(),
            });
        }

        let typed_decl = DestructuringDecl {
            targets: typed_targets,
            initializer: typed_initializer,
        };

        Ok(Some(TypedStatement {
            kind: TypedStatementKind::DestructuringDecl(typed_decl),
            span,
        }))
    }

    /// Analyzes an array destructuring declaration (`[a: Type, b: Type] = ...`).
    fn analyze_array_destructuring_decl(
        &mut self,
        targets: &[untyped::DestructuringTarget],
        initializer: &untyped::Expression,
        is_statement_mutable: bool,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let typed_initializer = self.analyze_expression(initializer, ctx, None, errors);
        if typed_initializer.jophet_type == JophetType::ErrorSentinel {
            return Ok(None);
        }

        let (member_type, size) =
            if let JophetType::Array { member_type, size } = &typed_initializer.jophet_type {
                (member_type.as_ref(), *size)
            } else {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Cannot destructure a value of type '{}' as an array.",
                        jophet_type_to_user_string(&typed_initializer.jophet_type)
                    ),
                    span: initializer.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            };

        let rest_pattern_exists = targets.iter().any(|t| t.is_rest_pattern);
        let num_explicit_targets = targets.len() - if rest_pattern_exists { 1 } else { 0 };

        if num_explicit_targets > size {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Too many variables in array destructuring. The pattern has {} variables, but the array has a size of {}.",
                    num_explicit_targets, size
                ),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        if !rest_pattern_exists && num_explicit_targets != size {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Mismatched arity in array destructuring. The pattern has {} variables, but the array has a size of {}. Array destructuring must be exhaustive without a rest pattern (`..`).",
                    num_explicit_targets, size
                ),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        let mut typed_targets = Vec::new();
        for target in targets {
            if target.is_rest_pattern || target.var_name == "_" {
                continue;
            }

            if is_statement_mutable && target.is_mutable {
                return Err(SemanticError::SyntaxError {
                   message: format!(
                       "Redundant `mutable` keyword for variable '{}'. When the entire declaration starts with `mutable`, all variables in the pattern are implicitly mutable.",
                       target.var_name
                   ),
                   span: span.clone(),
                   file_path: self.current_module_path.clone(),
               });
            }

            let declared_target_type = self.resolve_type(&target.ty, false, None, ctx, span.clone())?;
            if !self.is_type_compatible(&declared_target_type, member_type) {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Type mismatch for variable '{}' in array destructuring. Pattern expects type {}, but array members have type {}.",
                        target.var_name,
                        jophet_type_to_user_string(&declared_target_type),
                        jophet_type_to_user_string(member_type)
                    ),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            if ctx.symbol_table.contains_key(&target.var_name) {
                return Err(SemanticError::NameError {
                    message: format!("Redeclaration of variable '{}'", target.var_name),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            let final_target_mutability = is_statement_mutable || target.is_mutable;
            ctx.symbol_table.insert(
                target.var_name.clone(),
                super::context::SymbolInfo {
                    jophet_type: member_type.clone(),
                    is_mutable: final_target_mutability,
                    is_const: false,
                    mangled_name: None,
                },
            );

            typed_targets.push(TypedDestructuringTarget {
                var_name: target.var_name.clone(),
                jophet_type: member_type.clone(),
                is_mutable: final_target_mutability,
                source_field: None,
            });
        }

        let typed_decl = ArrayDestructuringDecl {
            targets: typed_targets,
            initializer: typed_initializer,
        };
        Ok(Some(TypedStatement {
            kind: TypedStatementKind::ArrayDestructuringDecl(typed_decl),
            span,
        }))
    }

    /// Analyzes a function declaration. This is a wrapper around `analyze_function_like_decl`.
    pub fn analyze_function_decl(
        &mut self,
        decl: &untyped::FunctionDecl,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        self.analyze_function_like_decl(decl, ctx, None, None, None, span, errors)
    }

    /// Analyzes a method declaration. This is a wrapper around `analyze_function_like_decl`.
    ///
    /// It provides the necessary context for analyzing a method, such as the receiver type name
    /// and an optional trait name. It returns an error if `analyze_function_like_decl`
    /// does not produce a statement, which can happen if a generic method template is
    /// analyzed without being monomorphized.
    pub fn analyze_method_decl(
        &mut self,
        decl: &untyped::FunctionDecl,
        struct_name: &str,
        trait_name: Option<&str>,
        ctx: &mut ScopeContext,
        force_mangled_name: Option<String>,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedStatement, SemanticError> {
        let typed_stmt = self
            .analyze_function_like_decl(
                decl,
                ctx,
                Some(struct_name),
                trait_name,
                force_mangled_name,
                span.clone(),
                errors,
            )?
            .ok_or_else(|| SemanticError::InternalError {
                message: "Method analysis failed to produce a typed declaration. This can happen if a generic method is analyzed without monomorphization.".to_string(),
                span,
                file_path: self.current_module_path.clone(),
            })?;
        Ok(typed_stmt)
    }

    /// Analyzes a function or method declaration. This is the central logic for function analysis.
    ///
    /// It handles generic parameter resolution, signature type checking, and body analysis.
    /// For generic function templates, it only performs a partial analysis to populate the
    /// symbol table. For concrete functions (including monomorphized instances), it performs
    /// a full analysis of the body. It correctly adds the function's own symbol to its
    /// local scope to enable recursion. It also handles optional return types, defaulting to `Nothing`,
    /// and inserts implicit `return nothing` statements. It now also refines the function's
    /// return type if it returns a specific closure instance.
    ///
    /// # Arguments
    /// * `force_mangled_name` - If `Some`, this name is used directly, overriding the default
    ///   name generation. This is crucial for monomorphization.
    pub(super) fn analyze_function_like_decl(
        &mut self,
        decl: &untyped::FunctionDecl,
        ctx: &mut ScopeContext,
        receiver_type_name: Option<&str>,
        trait_name: Option<&str>,
        force_mangled_name: Option<String>,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        // If this is a generic function template being analyzed during the initial collection pass,
        // we only register its signature and do not analyze the body or produce a typed statement.
        if !decl.generic_params.is_empty() && force_mangled_name.is_none() {
            let mut signature_ctx = ScopeContext::new();
            for gen_param in &decl.generic_params {
                signature_ctx
                    .generic_context
                    .insert(gen_param.name.clone(), vec![]);
            }
            let params_types: Vec<_> = decl
                .params
                .iter()
                .map(|(_, ty)| {
                    self.resolve_type(ty, false, receiver_type_name, &signature_ctx, span.clone())
                })
                .collect::<Result<_, _>>()?;
            let return_type = match &decl.return_type {
                Some(rt) => {
                    self.resolve_type(rt, false, receiver_type_name, &signature_ctx, span.clone())?
                }
                None => JophetType::Nothing,
            };

            let symbol_name = if let Some(rec) = receiver_type_name {
                format!("{}::{}", rec, decl.name)
            } else {
                decl.name.clone()
            };
            let mangled_name = if let Some(rec) = receiver_type_name {
                if let Some(t) = trait_name {
                    format!("{}_{}_{}", rec, t, decl.name)
                } else {
                    format!("{}_{}", rec, decl.name)
                }
            } else {
                format!(
                    "{}_{}",
                    self.current_module_path
                        .file_stem()
                        .unwrap()
                        .to_string_lossy(),
                    decl.name
                )
            };

            ctx.symbol_table.insert(
                symbol_name,
                super::context::SymbolInfo {
                    jophet_type: JophetType::Function {
                        params: params_types,
                        ret: Box::new(return_type),
                    },
                    is_mutable: false,
                    is_const: false,
                    mangled_name: Some(mangled_name),
                },
            );

            return Ok(None); // Do not produce a TypedStatement for the template.
        }

        let mut body_ctx = ScopeContext::new();
        body_ctx.symbol_table = ctx.symbol_table.clone();
        body_ctx.substitutions = ctx.substitutions.clone();

        let mut typed_generic_params = Vec::new();
        for gen_param in &decl.generic_params {
            let mut typed_bounds = Vec::new();
            for bound in &gen_param.bounds {
                typed_bounds.push(self.resolve_type(
                    bound,
                    false,
                    receiver_type_name,
                    &body_ctx,
                    span.clone(),
                )?);
            }
            typed_generic_params.push(TypedGenericParam {
                name: gen_param.name.clone(),
                bounds: typed_bounds.clone(),
            });
            body_ctx
                .generic_context
                .insert(gen_param.name.clone(), typed_bounds);
        }

        let return_type = match &decl.return_type {
            Some(rt) => {
                self.resolve_type(rt, false, receiver_type_name, &body_ctx, span.clone())?
            }
            None => JophetType::Nothing,
        };

        if matches!(
            return_type,
            JophetType::Reference(_) | JophetType::MutableReference(_)
        ) {
            return Err(SemanticError::TypeError {
                message: "Returning a reference to data owned by the current function is not allowed."
                    .to_string(),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        body_ctx.current_function_return_type = Some(return_type.clone());

        let (mangled_name, symbol_name) = if let Some(name) = force_mangled_name {
            let symbol_name = if let Some(rec) = receiver_type_name {
                format!("{}::{}", rec, decl.name)
            } else {
                decl.name.clone()
            };
            (name, symbol_name)
        } else {
            let symbol_name = if let Some(struct_name) = receiver_type_name {
                format!("{}::{}", struct_name, decl.name)
            } else {
                decl.name.clone()
            };
            let mangled = if let Some(struct_name) = receiver_type_name {
                if let Some(t_name) = trait_name {
                    format!("{}_{}_{}", struct_name, t_name, decl.name)
                } else {
                    format!("{}_{}", struct_name, decl.name)
                }
            } else {
                format!(
                    "{}_{}",
                    self.current_module_path
                        .file_stem()
                        .unwrap()
                        .to_string_lossy(),
                    decl.name
                )
            };
            (mangled, symbol_name)
        };

        let mut params = Vec::new();
        // If it's a closure, its first parameter is the environment pointer.
        if decl.name.is_empty() {
            let env_struct_name = format!("{}_env", mangled_name);
            let env_pointer_type = JophetType::Pointer(Box::new(JophetType::Struct {
                name: env_struct_name,
                module_path: PathBuf::new(),
            }));
            params.push(("env".to_string(), env_pointer_type));
        }

        if let Some(struct_name) = receiver_type_name {
            if decl.has_self {
                let self_type = self.resolve_type(
                    &untyped::Type::Simple(struct_name.to_string()),
                    false,
                    None,
                    &body_ctx,
                    span.clone(),
                )?;

                // Primitives are passed by value, aggregates by reference.
                let final_self_type = if self.is_primitive_for_self(&self_type) {
                    self_type
                } else {
                    JophetType::Reference(Box::new(self_type))
                };

                body_ctx.symbol_table.insert(
                    "self".to_string(),
                    super::context::SymbolInfo {
                        jophet_type: final_self_type.clone(),
                        is_mutable: false,
                        is_const: false,
                        mangled_name: None,
                    },
                );
                params.push(("self".to_string(), final_self_type));
            }
        }

        for (p_name, p_type) in &decl.params {
            let jophet_type =
                self.resolve_type(p_type, false, receiver_type_name, &body_ctx, span.clone())?;
            if jophet_type == JophetType::Nothing {
                return Err(SemanticError::TypeError {
                    message: format!("Function parameter '{}' cannot have type 'Nothing'.", p_name),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            body_ctx.symbol_table.insert(
                p_name.clone(),
                super::context::SymbolInfo {
                    jophet_type: jophet_type.clone(),
                    is_mutable: false,
                    is_const: false,
                    mangled_name: None,
                },
            );
            params.push((p_name.clone(), jophet_type));
        }

        // Add the function to the symbol table *before* analyzing the body to support recursion.
        let params_types: Vec<_> = params.iter().map(|(_, ty)| ty.clone()).collect();
        let func_symbol_info = super::context::SymbolInfo {
            jophet_type: JophetType::Function {
                params: params_types.clone(),
                ret: Box::new(return_type.clone()),
            },
            is_mutable: false,
            is_const: false,
            mangled_name: Some(mangled_name.clone()),
        };

        // Add to parent scope for other functions to call it.
        ctx.symbol_table
            .insert(symbol_name.clone(), func_symbol_info.clone());
        // ALSO add to the function's own scope for recursion.
        body_ctx
            .symbol_table
            .insert(symbol_name.clone(), func_symbol_info);

        let (mut body, _) =
            self.analyze_block(&decl.body, &mut body_ctx, Some(&return_type), false, errors);

        let mut concrete_return_type = return_type.clone();
        if let JophetType::Closure { .. } = &concrete_return_type {
            // Look for a return statement that provides a more specific closure type.
            for stmt in &body {
                if let TypedStatementKind::Return(expr) = &stmt.kind {
                    if let JophetType::Closure {
                        mangled_name, ..
                    } = &expr.jophet_type
                    {
                        if !mangled_name.is_empty() {
                            // We found it. This is the concrete type.
                            concrete_return_type = expr.jophet_type.clone();
                            break; // Assume the first one is representative for now.
                        }
                    }
                }
            }
        }

        // Update the symbol table with the refined return type.
        let final_func_symbol_info = super::context::SymbolInfo {
            jophet_type: JophetType::Function {
                params: params_types,
                ret: Box::new(concrete_return_type.clone()),
            },
            is_mutable: false,
            is_const: false,
            mangled_name: Some(mangled_name.clone()),
        };
        ctx.symbol_table.insert(symbol_name, final_func_symbol_info);

        // Insert an implicit `return nothing` if the function returns Nothing and doesn't
        // end with an explicit return statement.
        if concrete_return_type == JophetType::Nothing {
            let ends_with_return = match body.last().map(|s| &s.kind) {
                Some(TypedStatementKind::Return(_)) => true,
                _ => false,
            };
            if !ends_with_return {
                body.push(TypedStatement {
                    kind: TypedStatementKind::Return(TypedExpression {
                        kind: TypedExpressionKind::Literal(crate::core::ast::Literal::Nothing),
                        jophet_type: JophetType::Nothing,
                        span: span.clone(), // Use the function's span
                    }),
                    span: span.clone(),
                });
            }
        }

        if !body_ctx.ownership_map.is_empty() {
            let leaked_vars: Vec<_> = body_ctx.ownership_map.keys().cloned().collect();
            return Err(SemanticError::MemoryError {
                message: format!("Memory leak detected in function '{}'. The resources owned by the following variables are not moved or returned: {:?}", decl.name, leaked_vars),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        let typed_decl = TypedFunctionDecl {
            is_public: decl.is_public,
            is_const: decl.is_const,
            name: decl.name.clone(),
            doc_comment: decl.doc_comment.clone(),
            generic_params: typed_generic_params,
            mangled_name,
            params,
            return_type: concrete_return_type,
            body,
            receiver_type: receiver_type_name.map(String::from),
            captures: None,
        };

        Ok(Some(TypedStatement {
            kind: TypedStatementKind::FunctionDecl(typed_decl),
            span,
        }))
    }
}