// src/core/semantic_analyzer/statements.rs
//! Contains the semantic analysis logic for statements.
//!
//! This module handles the analysis of all statement types from the Untyped AST,
//! such as control flow (`if`, `while`), declarations, assignments, and returns.
//! It is responsible for type checking, enforcing semantic rules (e.g., `break`
//! only in loops), and translating the untyped statement nodes into their
//! fully-typed equivalents for the Typed AST. It now handles `if let`-style bindings
//! and enforces ownership rules for assignments and end-of-scope cleanup. All analysis
//! functions now collect errors in a vector instead of returning a `Result`.

use super::{types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer};
use crate::core::ast::typed::*;
use crate::core::ast::untyped::{self, AssignmentLValue, DeclarationPattern};
use crate::diagnostics::errors::{JophetError, SemanticError};
use std::collections::HashSet;

impl SemanticAnalyzer<'_> {
    /// The main entry point for analyzing a statement.
    ///
    /// It acts as a dispatcher, matching on the `untyped::StatementKind` and calling
    /// the appropriate specialized analysis function. It validates all semantic rules
    /// for the statement, such as type correctness, control flow validity, and definition
    /// correctness (e.g., ensuring structs do not contain borrow types).
    ///
    /// Some statements, like `import` and `implement`, are consumed during the initial
    /// setup pass and result in `Ok(None)`.
    ///
    /// # Arguments
    /// * `stmt` - The untyped statement to analyze.
    /// * `ctx` - The current `ScopeContext` for symbol lookups and state tracking.
    /// * `return_type` - The expected return type of the current function, if any.
    /// * `in_loop` - A flag indicating if the statement is inside a loop.
    /// * `errors` - The vector where any detected semantic errors will be stored.
    pub fn analyze_statement(
        &mut self,
        stmt: &untyped::Statement,
        ctx: &mut ScopeContext,
        return_type: Option<&JophetType>,
        in_loop: bool,
        errors: &mut Vec<SemanticError>,
    ) -> Option<TypedStatement> {
        let result = match &stmt.kind {
            // These are handled in the initial pass of the analyzer and don't produce a typed statement.
            untyped::StatementKind::Import { path } => {
                if let Err(e) = self.analyze_import(path, ctx, stmt.span.clone()) {
                    // Correctly handle the JophetError. We can downcast it to get the SemanticError.
                    if let JophetError::SemanticError(se) = e {
                        errors.push(se);
                    } else {
                        // If it's another kind of JophetError, we can't easily continue.
                        // For now, we'll wrap it in a generic SemanticError.
                        errors.push(SemanticError::ModuleError {
                            message: e.to_string(),
                            span: stmt.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                }
                Ok(None)
            }
            untyped::StatementKind::ImplementBlock(_) => Ok(None),

            untyped::StatementKind::Delete(name) => {
                self.analyze_delete_statement(name, ctx, stmt.span.clone())
            }

            // For definitions, this pass resolves all type annotations within them.
            untyped::StatementKind::StructDef(def) => {
                let mut typed_fields = Vec::new();
                for (name, ty, is_public, _) in &def.fields {
                    let jophet_type = match self.resolve_type(ty, true, Some(&def.name), ctx, stmt.span.clone()) {
                        Ok(t) => t,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    };

                    // Structs must own all their data. They cannot contain temporary borrows.
                    if matches!(
                    jophet_type,
                    JophetType::Reference(_) | JophetType::MutableReference(_)
                ) {
                    errors.push(SemanticError::TypeError {
                        message: format!(
                            "Struct field '{}' cannot have a borrow type. Structs must own all of their data.",
                            name
                        ),
                        span: stmt.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                    return None;
                }

                    typed_fields.push((name.clone(), jophet_type, *is_public));
                }

                let mut typed_generic_params = Vec::new();
                for p in &def.generic_params {
                    let bounds_result: Result<Vec<_>, _> = p
                        .bounds
                        .iter()
                        .map(|b| {
                            self.resolve_type(b, false, Some(&def.name), ctx, stmt.span.clone())
                        })
                        .collect();
                    
                    let typed_bounds = match bounds_result {
                        Ok(b) => b,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    };

                    typed_generic_params.push(TypedGenericParam {
                        name: p.name.clone(),
                        bounds: typed_bounds,
                    });
                }

                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::StructDef(TypedStructDef {
                        is_public: def.is_public,
                        name: def.name.clone(),
                        doc_comment: def.doc_comment.clone(),
                        generic_params: typed_generic_params,
                        fields: typed_fields,
                        module_path: def.module_path.clone(),
                    }),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::EnumDef(def) => {
                let mut typed_members = Vec::new();
                let mut next_value = 0i64;
                for (name, value_opt, doc_comment) in &def.members {
                    let current_value = match value_opt {
                        Some(val) => *val,
                        None => next_value,
                    };
                    typed_members.push((name.clone(), current_value, doc_comment.clone()));
                    next_value = current_value + 1;
                }

                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::EnumDef(TypedEnumDef {
                        is_public: def.is_public,
                        name: def.name.clone(),
                        doc_comment: def.doc_comment.clone(),
                        members: typed_members,
                        module_path: def.module_path.clone(),
                    }),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::UnionDef(def) => {
                let mut typed_fields = Vec::new();
                for (name, ty, doc_comment) in &def.fields {
                    let jophet_type = match self.resolve_type(ty, true, Some(&def.name), ctx, stmt.span.clone()) {
                        Ok(t) => t,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    };
                    typed_fields.push((name.clone(), jophet_type, doc_comment.clone()));
                }
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::UnionDef(TypedUnionDef {
                        is_public: def.is_public,
                        name: def.name.clone(),
                        doc_comment: def.doc_comment.clone(),
                        fields: typed_fields,
                        module_path: def.module_path.clone(),
                    }),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::TaggedUnionDef(def) => {
                let mut typed_variants = Vec::new();
                for variant in &def.variants {
                    let payload_type = match variant
                        .payload
                        .as_ref()
                        .map(|p| self.resolve_type(p, true, Some(&def.name), ctx, stmt.span.clone()))
                        .transpose()
                    {
                        Ok(t) => t,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    };
                    typed_variants.push(TypedTaggedUnionVariant {
                        name: variant.name.clone(),
                        doc_comment: variant.doc_comment.clone(),
                        payload: payload_type,
                    });
                }

                let mut typed_generic_params = Vec::new();
                    for p in &def.generic_params {
                        let bounds_result: Result<Vec<_>, _> = p
                            .bounds
                            .iter()
                            .map(|b| {
                                self.resolve_type(b, false, Some(&def.name), ctx, stmt.span.clone())
                            })
                            .collect();

                        let typed_bounds = match bounds_result {
                            Ok(b) => b,
                            Err(e) => {
                                errors.push(e);
                                return None;
                            }
                        };
                        
                        typed_generic_params.push(TypedGenericParam {
                            name: p.name.clone(),
                            bounds: typed_bounds,
                        });
                    }

                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::TaggedUnionDef(TypedTaggedUnionDef {
                        is_public: def.is_public,
                        name: def.name.clone(),
                        doc_comment: def.doc_comment.clone(),
                        generic_params: typed_generic_params,
                        variants: typed_variants,
                        module_path: def.module_path.clone(),
                    }),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::ErrorDef(def) => {
                let mut typed_variants = Vec::new();
                for variant in &def.variants {
                    let payload_type = match variant
                        .payload
                        .as_ref()
                        .map(|p| self.resolve_type(p, true, Some(&def.name), ctx, stmt.span.clone()))
                        .transpose()
                    {
                        Ok(t) => t,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    };
                    typed_variants.push(TypedTaggedUnionVariant {
                        name: variant.name.clone(),
                        doc_comment: variant.doc_comment.clone(),
                        payload: payload_type,
                    });
                }
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::ErrorDef(TypedErrorDef {
                        is_public: def.is_public,
                        name: def.name.clone(),
                        doc_comment: def.doc_comment.clone(),
                        variants: typed_variants,
                        module_path: def.module_path.clone(),
                    }),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::TraitDef(def) => {
                let mut typed_methods = Vec::new();
                for method_decl in &def.methods {
                    match self.analyze_function_like_decl(
                        method_decl,
                        ctx,
                        Some(&def.name),
                        Some(&def.name),
                        None,
                        stmt.span.clone(),
                        errors,
                    ) {
                        Ok(Some(TypedStatement {
                            kind: TypedStatementKind::FunctionDecl(typed_decl),
                            ..
                        })) => {
                            typed_methods.push(typed_decl);
                        }
                        Ok(Some(_)) => {
                            // This case should ideally not happen when analyzing a function,
                            // but we handle it to make the match exhaustive.
                        }
                        Ok(None) => { /* Function was generic, already handled */ }
                        Err(e) => errors.push(e),
                    };
                }

                let mut typed_generic_params = Vec::new();
                for p in &def.generic_params {
                    let bounds_result: Result<Vec<_>, _> = p
                        .bounds
                        .iter()
                        .map(|b| {
                            self.resolve_type(b, false, Some(&def.name), ctx, stmt.span.clone())
                        })
                        .collect();

                    let typed_bounds = match bounds_result {
                        Ok(b) => b,
                        Err(e) => {
                            errors.push(e);
                            return None;
                        }
                    };
                    
                    typed_generic_params.push(TypedGenericParam {
                        name: p.name.clone(),
                        bounds: typed_bounds,
                    });
                }

                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::TraitDef(TypedTraitDef {
                        is_public: def.is_public,
                        name: def.name.clone(),
                        doc_comment: def.doc_comment.clone(),
                        generic_params: typed_generic_params,
                        methods: typed_methods,
                        module_path: def.module_path.clone(),
                    }),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::VariableDecl(decl) => {
                self.analyze_variable_decl(decl, ctx, stmt.span.clone(), errors)
            }
            untyped::StatementKind::FunctionDecl(decl) => {
                self.analyze_function_decl(decl, ctx, stmt.span.clone(), errors)
            }
            untyped::StatementKind::If(if_stmt) => {
                self.analyze_if_statement(if_stmt, ctx, return_type, in_loop, stmt.span.clone(), errors)
            }
            untyped::StatementKind::While(while_stmt) => {
                self.analyze_while_statement(while_stmt, ctx, return_type, stmt.span.clone(), errors)
            }
            untyped::StatementKind::For(for_stmt) => {
                self.analyze_for_statement(for_stmt, ctx, return_type, stmt.span.clone(), errors)
            }

            untyped::StatementKind::Break => {
                if !in_loop {
                    errors.push(SemanticError::FlowError {
                        message: "'break' can only be used inside a loop".to_string(),
                        span: stmt.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                    return None;
                }
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::Break,
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::Continue => {
                if !in_loop {
                    errors.push(SemanticError::FlowError {
                        message: "'continue' can only be used inside a loop".to_string(),
                        span: stmt.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                    return None;
                }
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::Continue,
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::ExpressionStatement(expr) => {
                let typed_expr = self.analyze_expression(expr, ctx, None, errors);
                if typed_expr.jophet_type == JophetType::ErrorSentinel {
                    return None;
                }
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::ExpressionStatement(typed_expr),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::Return(expr) => {
                let expected_type = match return_type {
                    Some(t) => t,
                    None => {
                        errors.push(SemanticError::FlowError {
                            message: "Return statement outside of a function".to_string(),
                            span: stmt.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                        return None;
                    }
                };
                let return_expr = self.analyze_expression(expr, ctx, Some(expected_type), errors);
                if return_expr.jophet_type == JophetType::ErrorSentinel {
                    return None;
                }
                let wrapped_expr = self.auto_wrap_if_needed(return_expr, expected_type);

                if !self.is_type_compatible(&wrapped_expr.jophet_type, expected_type) {
                    errors.push(SemanticError::TypeError {
                        message: format!(
                            "Mismatched return type. Expected {}, found {}",
                            jophet_type_to_user_string(expected_type),
                            jophet_type_to_user_string(&wrapped_expr.jophet_type)
                        ),
                        span: expr.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                    return None;
                }
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::Return(wrapped_expr),
                    span: stmt.span.clone(),
                }))
            }
            untyped::StatementKind::Assignment(left, right) => {
                self.analyze_assignment(left, right, ctx, stmt.span.clone(), errors)
            }
            untyped::StatementKind::Yield(expr) => self.analyze_yield(expr, ctx, stmt.span.clone()),
        };

        match result {
            Ok(maybe_stmt) => maybe_stmt,
            Err(e) => {
                errors.push(e);
                None
            }
        }
    }

    /// Analyzes an immediate `delete` statement, enforcing memory safety rules.
    fn analyze_delete_statement(
        &self,
        name: &str,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let info = ctx
            .symbol_table
            .get(name)
            .ok_or_else(|| SemanticError::NameError {
                message: format!("Undefined variable `{}`", name),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            })?;

        if let Some(state) = ctx.borrow_states.get(name) {
            if *state != super::context::BorrowState::Unique {
                return Err(SemanticError::MemoryError {
                    message: format!("Cannot delete `{}` because it is currently borrowed", name),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        if ctx.ownership_map.remove(name).is_none() {
            if ctx.moved_vars.contains(name) {
                return Err(SemanticError::MemoryError {
                    message: format!(
                        "Invalid delete: Cannot delete '{}' because its value has already been moved or deleted.",
                        name
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            } else {
                return Err(SemanticError::MemoryError {
                    message: format!(
                        "Invalid delete: '{}' is a stack-allocated variable. `delete` can only be used on values created with `new`.",
                        name
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        ctx.deleted_vars.insert(name.to_string());

        if !self.is_heap_type(&info.jophet_type) {
            return Err(SemanticError::MemoryError {
                message: format!(
                    "Cannot use `delete` on a variable of type {:?}, which is not heap-allocated.",
                    info.jophet_type
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }
        let typed_stmt = TypedStatement {
            kind: TypedStatementKind::Delete(name.to_string(), info.jophet_type.clone()),
            span,
        };
        Ok(Some(typed_stmt))
    }

    /// Analyzes an `if` statement, recursively analyzing its branches. This now handles
    /// the `if let`-style binding for unwrapping fallible types.
    fn analyze_if_statement(
        &mut self,
        if_stmt: &untyped::IfStatement,
        ctx: &mut ScopeContext,
        return_type: Option<&JophetType>,
        in_loop: bool,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        if let Some((var_name, var_type)) = &if_stmt.binding {
            // This is an `if let`-style binding.
            let typed_initializer = self.analyze_expression(&if_stmt.condition, ctx, None, errors);
            if typed_initializer.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
            let declared_type = self.resolve_type(var_type, false, None, ctx, span.clone())?;

            let ok_type = if let JophetType::Fallible { ok, .. } = &typed_initializer.jophet_type {
                ok.as_ref()
            } else {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "The expression in an `if let` statement must be a fallible type (Type?), but found '{}'.",
                        jophet_type_to_user_string(&typed_initializer.jophet_type)
                    ),
                    span: if_stmt.condition.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            };

            if declared_type != *ok_type {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Type mismatch in `if let` binding. The expression produces a value of type '{}', but the variable is declared as '{}'.",
                        jophet_type_to_user_string(ok_type),
                        jophet_type_to_user_string(&declared_type)
                    ),
                    span: span.clone(), // This could be improved to span just the type annotation
                    file_path: self.current_module_path.clone(),
                });
            }

            // Create a new scope for the `then` block and add the unwrapped variable.
            let mut then_ctx = ctx.clone();
            then_ctx.symbol_table.insert(
                var_name.clone(),
                super::SymbolInfo {
                    jophet_type: declared_type.clone(),
                    is_mutable: false,
                    is_const: false,
                    mangled_name: None,
                },
            );

            let (then_block, _) = self.analyze_block(&if_stmt.then_block, &mut then_ctx, return_type, in_loop, errors);
            
            // The `else` block is analyzed in the original context, without the new variable.
            let else_block = if_stmt
                .else_block
                .as_ref()
                .map(|eb| self.analyze_else_block(eb, ctx, return_type, in_loop, errors))
                .transpose()?;

            let typed_if = TypedIfStatement {
                condition: typed_initializer,
                binding: Some((var_name.clone(), declared_type)),
                then_block,
                else_block,
            };

            Ok(Some(TypedStatement {
                kind: TypedStatementKind::If(typed_if),
                span,
            }))

        } else {
            // This is a standard `if` with a boolean condition.
            let condition = self.analyze_expression(&if_stmt.condition, ctx, Some(&JophetType::Bool), errors);
            if condition.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
            if condition.jophet_type != JophetType::Bool {
                return Err(SemanticError::TypeError {
                    message: "If condition must be a boolean".to_string(),
                    span: if_stmt.condition.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
            let (then_block, _) = self.analyze_block(&if_stmt.then_block, ctx, return_type, in_loop, errors);
            let else_block = if_stmt
                .else_block
                .as_ref()
                .map(|eb| self.analyze_else_block(eb, ctx, return_type, in_loop, errors))
                .transpose()?;

            let typed_if = TypedIfStatement {
                condition,
                binding: None,
                then_block,
                else_block,
            };
            Ok(Some(TypedStatement {
                kind: TypedStatementKind::If(typed_if),
                span,
            }))
        }
    }

    /// Analyzes an `else` or `else if` block.
    fn analyze_else_block(
        &mut self,
        else_block: &untyped::ElseBlock,
        ctx: &mut ScopeContext,
        return_type: Option<&JophetType>,
        in_loop: bool,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Box<TypedElseBlock>, SemanticError> {
        match else_block {
            untyped::ElseBlock::Else(stmts) => {
                let (typed_stmts, _) = self.analyze_block(stmts, ctx, return_type, in_loop, errors);
                Ok(Box::new(TypedElseBlock::Else(typed_stmts)))
            }
            untyped::ElseBlock::ElseIf(if_stmt) => {
                let else_if_span = if_stmt.condition.span.clone();
                let typed_if_opt =
                    self.analyze_if_statement(if_stmt, ctx, return_type, in_loop, else_if_span, errors)?;
                let typed_if = typed_if_opt.unwrap();
                if let TypedStatementKind::If(tif) = typed_if.kind {
                    Ok(Box::new(TypedElseBlock::ElseIf(tif)))
                } else {
                    unreachable!()
                }
            }
        }
    }

    /// Analyzes a `while` statement.
    fn analyze_while_statement(
        &mut self,
        while_stmt: &untyped::WhileStatement,
        ctx: &mut ScopeContext,
        return_type: Option<&JophetType>,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let condition =
            self.analyze_expression(&while_stmt.condition, ctx, Some(&JophetType::Bool), errors);
        if condition.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
        if condition.jophet_type != JophetType::Bool {
            return Err(SemanticError::TypeError {
                message: "While condition must be a boolean".to_string(),
                span: while_stmt.condition.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }
        let (body, _) = self.analyze_block(&while_stmt.body, ctx, return_type, true, errors);
        let typed_while = TypedWhileStatement { condition, body };
        Ok(Some(TypedStatement {
            kind: TypedStatementKind::While(typed_while),
            span,
        }))
    }

    /// Analyzes a `for` loop, handling both numeric range and iterable-based loops.
    fn analyze_for_statement(
        &mut self,
        for_stmt: &untyped::ForStatement,
        ctx: &mut ScopeContext,
        return_type: Option<&JophetType>,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        match &for_stmt.kind {
            untyped::ForLoopKind::Range { start, stop, step } => {
                let typed_start = self.analyze_expression(start, ctx, None, errors);
                if typed_start.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
                let typed_stop = self.analyze_expression(stop, ctx, None, errors);
                if typed_stop.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
                let typed_step = step
                    .as_ref()
                    .map(|s| self.analyze_expression(s, ctx, None, errors));
                
                if let Some(ref ts) = typed_step {
                    if ts.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
                }

                let iterator_type = typed_start.jophet_type.clone();

                let mut body_ctx = ctx.clone();
                body_ctx.symbol_table.insert(
                    for_stmt.iterator_name.clone(),
                    super::context::SymbolInfo {
                        jophet_type: iterator_type.clone(),
                        is_mutable: false,
                        is_const: false,
                        mangled_name: None,
                    },
                );

                let (body, _) = self.analyze_block(&for_stmt.body, &mut body_ctx, return_type, true, errors);

                let typed_for = TypedForStatement {
                    iterator_name: for_stmt.iterator_name.clone(),
                    iterator_type,
                    start: typed_start,
                    stop: typed_stop,
                    step: typed_step,
                    body,
                };
                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::For(typed_for),
                    span,
                }))
            }
            untyped::ForLoopKind::Iterable { collection } => {
                let typed_collection = self.analyze_expression(collection, ctx, None, errors);
                if typed_collection.jophet_type == JophetType::ErrorSentinel { return Ok(None); }

                let iterator_type = match &typed_collection.jophet_type {
                    JophetType::Array { member_type, .. } => *member_type.clone(),
                    JophetType::Vector(member_type) => *member_type.clone(),
                    JophetType::String => JophetType::Char,
                    JophetType::PythonObject { .. } => JophetType::PythonObject { brand: Box::new(self.py_any_brand.clone()) },
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Cannot iterate over type '{}'. Only Array, Vector, String, and PythonObject are iterable.",
                                jophet_type_to_user_string(&typed_collection.jophet_type)
                            ),
                            span: collection.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                };

                let mut body_ctx = ctx.clone();
                body_ctx.symbol_table.insert(
                    for_stmt.iterator_name.clone(),
                    super::context::SymbolInfo {
                        jophet_type: iterator_type.clone(),
                        is_mutable: false,
                        is_const: false,
                        mangled_name: None,
                    },
                );

                let (body, _) = self.analyze_block(&for_stmt.body, &mut body_ctx, return_type, true, errors);

                let typed_for_in = TypedForInStatement {
                    iterator_name: for_stmt.iterator_name.clone(),
                    iterator_type,
                    collection: typed_collection,
                    body,
                };

                Ok(Some(TypedStatement {
                    kind: TypedStatementKind::ForIn(typed_for_in),
                    span,
                }))
            }
        }
    }

    /// Analyzes an assignment statement, enforcing mutability and borrow checking rules.
    /// This now handles simple, tuple destructuring, and array destructuring assignments,
    /// as well as enforcing move semantics for owned types.
    fn analyze_assignment(
        &mut self,
        left: &untyped::AssignmentLValue,
        right: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        match left {
            AssignmentLValue::Expression(left_expr) => {
                self.analyze_simple_assignment(left_expr, right, ctx, span, errors)
            }
            AssignmentLValue::Tuple(names) => {
                self.analyze_tuple_destructuring_assignment(names, right, ctx, span, errors)
            }
            AssignmentLValue::Array(names) => {
                self.analyze_array_destructuring_assignment(names, right, ctx, span, errors)
            }
        }
    }

    /// Analyzes a simple (non-destructuring) assignment, enforcing move semantics and providing improved error messages for immutability violations.
    fn analyze_simple_assignment(
        &mut self,
        left: &untyped::Expression,
        right: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let mut typed_left = self.analyze_expression(left, ctx, None, errors);
        if typed_left.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
        let typed_right = self.analyze_expression(right, ctx, Some(&typed_left.jophet_type), errors);
        if typed_right.jophet_type == JophetType::ErrorSentinel { return Ok(None); }

        let (is_assignable, name) = self.is_assignable(&mut typed_left, ctx)?;
        if !is_assignable {
            let mut message = format!("Cannot assign to immutable variable '{}'", name);
            // Check if it's a simple variable to provide the "make mutable" hint.
            if matches!(&left.kind, untyped::ExpressionKind::Identifier(_)) {
                message.push_str(&format!(
                    ". Help: To allow mutation, declare the variable using `mutable`, for example: `mutable {} ...`.",
                    name
                ));
            }

            return Err(SemanticError::TypeError {
                message,
                span: left.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        if let TypedExpressionKind::Identifier { name: ident_name, .. } = &typed_left.kind {
            if let Some(state) = ctx.borrow_states.get(ident_name) {
                if *state != super::context::BorrowState::Unique {
                    return Err(SemanticError::MemoryError {
                        message: format!(
                            "Cannot assign to `{}` because it is currently borrowed",
                            ident_name
                        ),
                        span: left.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
            }
            if ctx.ownership_map.contains_key(ident_name) {
                return Err(SemanticError::MemoryError {
                    message: format!(
                        "Memory leak: Cannot assign to '{}' because it already owns a value. Use `delete {}` first.",
                        ident_name, ident_name
                    ),
                    span: left.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }
        
        // Enforce move semantics on assignment.
        if self.is_owned_type(&typed_right.jophet_type) {
            if let TypedExpressionKind::Identifier { name: source_name, .. } = &typed_right.kind {
                // If the source is being cloned, it's not a move. Otherwise, ownership is transferred.
                if !matches!(&right.kind, untyped::ExpressionKind::FunctionCall { name, .. } if name == "clone") {
                    if ctx.ownership_map.contains_key(source_name) {
                        ctx.moved_vars.insert(source_name.clone());
                    }
                }
            }
        }

        if let (
            TypedExpressionKind::Identifier { name: dest_name, .. },
            TypedExpressionKind::Identifier { name: source_name, .. },
        ) = (&typed_left.kind, &typed_right.kind)
        {
            if let Some(alloc_id) = ctx.ownership_map.remove(source_name) {
                ctx.ownership_map.insert(dest_name.clone(), alloc_id);
            }
        }

        let mut wrapped_right = self.auto_wrap_if_needed(typed_right, &typed_left.jophet_type);
        if let JophetType::String = &typed_left.jophet_type {
            if let JophetType::StringSlice = &wrapped_right.jophet_type {
                wrapped_right = TypedExpression {
                    kind: TypedExpressionKind::New {
                        jophet_type: JophetType::String,
                        args: vec![wrapped_right],
                    },
                    jophet_type: JophetType::String,
                    span: right.span.clone(),
                }
            }
        }

        if wrapped_right.jophet_type != typed_left.jophet_type {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Type mismatch in assignment. Expected {:?}, found {:?}",
                    typed_left.jophet_type, wrapped_right.jophet_type
                ),
                span: right.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }
        Ok(Some(TypedStatement {
            kind: TypedStatementKind::Assignment(
                TypedAssignmentLValue::Expression(typed_left),
                wrapped_right,
            ),
            span,
        }))
    }
    
    /// Analyzes a tuple destructuring assignment.
    fn analyze_tuple_destructuring_assignment(
        &mut self,
        names: &[String],
        right: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let typed_initializer = self.analyze_expression(right, ctx, None, errors);
        if typed_initializer.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
        let element_types = if let JophetType::Tuple(types) = &typed_initializer.jophet_type {
            types
        } else {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Cannot destructure a value of type '{}' as a tuple.",
                    jophet_type_to_user_string(&typed_initializer.jophet_type)
                ),
                span: right.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        };

        if names.len() != element_types.len() {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Mismatched arity in destructuring assignment. The pattern has {} variables, but the tuple has {} elements.",
                    names.len(),
                    element_types.len()
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }
        
        let mut typed_targets = Vec::new();
        for (i, name) in names.iter().enumerate() {
            // Re-analyze each name as an expression to get its typed AST node.
            // This is necessary to correctly handle assignments to fields, etc.
            let untyped_target = untyped::Expression {
                kind: untyped::ExpressionKind::Identifier(name.clone()),
                span: span.clone(), // This span is approximate
            };
            let mut typed_target = self.analyze_expression(&untyped_target, ctx, None, errors);
            if typed_target.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
            let (is_assignable, _) = self.is_assignable(&mut typed_target, ctx)?;

            if !is_assignable {
                 return Err(SemanticError::TypeError {
                    message: format!("Cannot assign to immutable variable '{}' in destructuring assignment.", name),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            if typed_target.jophet_type != element_types[i] {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Type mismatch for variable '{}' in destructuring assignment. Variable has type {}, but tuple element has type {}.",
                        name,
                        jophet_type_to_user_string(&typed_target.jophet_type),
                        jophet_type_to_user_string(&element_types[i])
                    ),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
            typed_targets.push(typed_target);
        }

        Ok(Some(TypedStatement {
            kind: TypedStatementKind::Assignment(
                TypedAssignmentLValue::Tuple(typed_targets),
                typed_initializer,
            ),
            span,
        }))
    }

    /// Analyzes an array destructuring assignment.
    fn analyze_array_destructuring_assignment(
        &mut self,
        names: &[String],
        right: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let typed_initializer = self.analyze_expression(right, ctx, None, errors);
        if typed_initializer.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
        let (member_type, size) = if let JophetType::Array { member_type, size } = &typed_initializer.jophet_type {
            (member_type, size)
        } else {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Cannot destructure a value of type '{}' as an array.",
                    jophet_type_to_user_string(&typed_initializer.jophet_type)
                ),
                span: right.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        };

        if names.len() != *size {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Mismatched arity in array destructuring assignment. The pattern has {} variables, but the array has a size of {}.",
                    names.len(),
                    size
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        let mut typed_targets = Vec::new();
        for name in names {
            let untyped_target = untyped::Expression {
                kind: untyped::ExpressionKind::Identifier(name.clone()),
                span: span.clone(), // Approximate span
            };
            let mut typed_target = self.analyze_expression(&untyped_target, ctx, None, errors);
            if typed_target.jophet_type == JophetType::ErrorSentinel { return Ok(None); }
            let (is_assignable, _) = self.is_assignable(&mut typed_target, ctx)?;

            if !is_assignable {
                 return Err(SemanticError::TypeError {
                    message: format!("Cannot assign to immutable variable '{}' in destructuring assignment.", name),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            if typed_target.jophet_type != **member_type {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Type mismatch for variable '{}' in array destructuring assignment. Variable has type {}, but the array's member type is {}.",
                        name,
                        jophet_type_to_user_string(&typed_target.jophet_type),
                        jophet_type_to_user_string(member_type)
                    ),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
            typed_targets.push(typed_target);
        }

        Ok(Some(TypedStatement {
            kind: TypedStatementKind::Assignment(
                TypedAssignmentLValue::Array(typed_targets),
                typed_initializer,
            ),
            span,
        }))
    }

    /// Analyzes a `yield` statement from a `switch` or `catch` expression.
    fn analyze_yield(
        &mut self,
        expr: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
    ) -> Result<Option<TypedStatement>, SemanticError> {
        let current_yield_type = ctx.current_switch_yield_type.clone().ok_or_else(|| {
            SemanticError::FlowError {
                message: "'yield' can only be used inside a switch or catch expression"
                    .to_string(),
                span: span.clone(),
                file_path: self.current_module_path.clone(),
            }
        })?;

        let mut temp_errors = Vec::new();
        let typed_expr = self.analyze_expression(
            expr,
            ctx,
            Some(&current_yield_type).filter(|t| **t != JophetType::Nothing),
            &mut temp_errors,
        );

        if !temp_errors.is_empty() {
            // To prevent cascading, we return the first error found during expression analysis.
            return Err(temp_errors.remove(0));
        }

        if current_yield_type == JophetType::Nothing {
            ctx.current_switch_yield_type = Some(typed_expr.jophet_type.clone());
        } else if let Some(upgraded_type) =
            self.try_upgrade_to_fallible(&current_yield_type, &typed_expr.jophet_type)
        {
            ctx.current_switch_yield_type = Some(upgraded_type);
        } else if let Some(upgraded_type) =
            self.try_upgrade_to_fallible(&typed_expr.jophet_type, &current_yield_type)
        {
            ctx.current_switch_yield_type = Some(upgraded_type);
        } else if !self.is_type_compatible(&typed_expr.jophet_type, &current_yield_type)
            && !self.is_type_compatible(&current_yield_type, &typed_expr.jophet_type)
        {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Mismatched types in switch/catch branches. Expected compatible with {:?}, found {:?}",
                    current_yield_type, typed_expr.jophet_type
                ),
                span: expr.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }
        Ok(Some(TypedStatement {
            kind: TypedStatementKind::Yield(typed_expr),
            span,
        }))
    }
}