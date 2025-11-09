// src/core/semantic_analyzer/expressions/const_eval.rs
//! Contains helper functions for compile-time evaluation.
//!
//! This module provides the logic for determining if an expression is a compile-time
//! constant and for converting the results of compile-time execution back into
//! literal AST nodes. It uses a robust, demand-driven approach for evaluating dependencies,
//! recursively triggering the analysis of non-`const` variables if a `const` context
//! requires their value at compile time.
//!
//! The main evaluation is spawned in a new thread with a larger stack to prevent
//! stack overflows during deep recursion (e.g., `const fib(93)`). All subsequent
//! recursive evaluations happen within this single thread, sharing a unified state.

use crate::backend::{BackendType, TargetInfo};
use crate::core::ast::typed::*;
use crate::core::ast::untyped::{self, DeclarationPattern};
use crate::core::ast::Literal;
use crate::core::ctfe::{interpreter::Interpreter, ComptimeValue, CtfeError};
use crate::core::semantic_analyzer::{
    types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer,
};
use crate::diagnostics::errors::SemanticError;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::thread;

thread_local! {
    /// A flag to prevent nested thread spawns during recursive compile-time evaluation.
    static IN_CTFE_THREAD: Cell<bool> = Cell::new(false);
}

impl<'a> SemanticAnalyzer<'a> {
    /// The internal engine for compile-time evaluation, orchestrating the interpreter
    /// and the demand-driven analysis of dependencies.
    ///
    /// This function's role is now simplified. It is responsible for creating an `Interpreter`
    /// instance and starting the top-level evaluation. The interpreter itself now handles
    /// the recursive, demand-driven analysis of any dependencies by calling back into the
    /// `SemanticAnalyzer` to which it holds a mutable reference.
    fn evaluate_const_expression_engine(
        &mut self,
        expr: &TypedExpression,
        analyzer_ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
        cache: &RefCell<HashMap<String, ComptimeValue>>,
    ) -> Result<ComptimeValue, CtfeError> {
        // The context for local variables within the compile-time evaluation.
        // For a top-level expression, it starts empty. For a function call, it's populated with args.
        let mut ctfe_ctx = HashMap::new();

        // Clone the function maps first to release the immutable borrow on `self` before
        // creating the mutable borrow that gets passed to the Interpreter.
        let monomorphized_functions = self.monomorphized_functions.borrow().clone();
        let imported_functions = self.imported_functions.clone();

        let mut interpreter = Interpreter::new(
            monomorphized_functions,
            imported_functions,
            cache,
            self, // Pass the mutable reference to the analyzer.
        );

        // Correctly pass the local `ctfe_ctx`, the `analyzer_ctx`, and the `errors` vector.
        // The loop is no longer needed here as the demand-driven logic is now inside the interpreter.
        interpreter.evaluate_expression(
            expr,
            &mut ctfe_ctx, // The local context for the interpreter's execution frame.
            analyzer_ctx,  // The global/shared context.
            errors,
        )
    }

    /// Public entry point to the CTFE engine. It attempts to evaluate a typed expression at compile time.
    ///
    /// To prevent stack overflows from deeply recursive `const` functions, this function
    /// checks if it's already in a dedicated CTFE thread. If not, it spawns one. All
    /// subsequent recursive evaluations for dependencies will occur within that single thread.
    pub fn try_evaluate_at_compile_time(
        &mut self,
        expr: &TypedExpression,
        analyzer_ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<ComptimeValue, CtfeError> {
        // If we are already inside the CTFE worker thread, we must not spawn another one.
        // Instead, we call the engine directly to handle the recursive evaluation.
        if IN_CTFE_THREAD.get() {
            let cache = RefCell::new(HashMap::new()); // A new cache for this sub-evaluation is fine.
            return self.evaluate_const_expression_engine(
                expr,
                analyzer_ctx,
                errors,
                &cache,
            );
        }

        // --- Top-level call: Spawn a worker thread with a larger stack ---

        // Clone all necessary state to move into the new thread.
        let thread_expr = expr.clone();
        let mut thread_analyzer_ctx = analyzer_ctx.clone();
        let mut thread_errors = errors.clone();
        let thread_target_info = self.target_info.clone();

        // Clone all definition maps.
        let thread_monomorphized_functions = self.monomorphized_functions.borrow().clone();
        let thread_imported_functions = self.imported_functions.clone();
        let thread_struct_defs = self.struct_defs.clone();
        let thread_enum_defs = self.enum_defs.clone();
        let thread_union_defs = self.union_defs.clone();
        let thread_tagged_union_defs = self.tagged_union_defs.clone();
        let thread_error_defs = self.error_defs.clone();
        let thread_trait_defs = self.trait_defs.clone();
        let thread_trait_impls = self.trait_impls.clone();
        let thread_inherent_impl_blocks = self.inherent_impl_blocks.clone();
        let thread_generic_functions = self.generic_functions.clone();
        let thread_monomorphized_structs = self.monomorphized_structs.borrow().clone();
        let thread_current_module_path = self.current_module_path.clone();

        let builder = thread::Builder::new()
            .name("ctfe_engine".into())
            .stack_size(16 * 1024 * 1024); // 16MB stack

        let handle = builder.spawn(move || {
            // Set the thread-local flag to indicate we are inside the CTFE worker thread.
            IN_CTFE_THREAD.set(true);

            // Re-create a temporary analyzer instance inside the new thread.
            // This instance will be used for all recursive analysis calls within the thread.
            let mut temp_analyzer = SemanticAnalyzer {
                struct_defs: thread_struct_defs,
                enum_defs: thread_enum_defs,
                union_defs: thread_union_defs,
                tagged_union_defs: thread_tagged_union_defs,
                error_defs: thread_error_defs,
                all_error_types: HashSet::new(),
                trait_defs: thread_trait_defs,
                trait_impls: thread_trait_impls,
                inherent_impl_blocks: thread_inherent_impl_blocks,
                generic_functions: thread_generic_functions,
                monomorphized_functions: RefCell::new(thread_monomorphized_functions),
                monomorphized_structs: RefCell::new(thread_monomorphized_structs),
                modules: HashMap::new(),
                imported_functions: thread_imported_functions,
                processed_imports: HashSet::new(),
                linked_libs: HashSet::new(),
                project_root: PathBuf::new(),
                current_module_path: thread_current_module_path,
                shared_deps_dir: &PathBuf::new(),
                is_release_build: false,
                keep_intermediate: false,
                backend_type: BackendType::C,
                closure_counter: 0,
                closure_cache: HashMap::new(),
                needs_python_runtime: false,
                py_any_brand: JophetType::ErrorSentinel,
                target_info: &thread_target_info,
            };

            // This cache is shared across all recursive calls within this thread.
            let cache = RefCell::new(HashMap::new());
            
            // Make the single top-level call to the evaluation engine.
            match temp_analyzer.evaluate_const_expression_engine(
                &thread_expr,
                &mut thread_analyzer_ctx,
                &mut thread_errors,
                &cache,
            ) {
                Ok(value) => Ok((value, thread_analyzer_ctx, thread_errors)),
                Err(e) => Err((e, thread_errors)),
            }
        }).unwrap();

        // Join the thread and propagate its result.
        match handle.join().unwrap() {
            Ok((value, updated_ctx, thread_errs)) => {
                // Merge the updated state back into the main analyzer's context.
                analyzer_ctx.comptime_values = updated_ctx.comptime_values;
                *errors = thread_errs;
                Ok(value)
            }
            Err((e, thread_errs)) => {
                *errors = thread_errs;
                Err(e)
            }
        }
    }

    /// Recursively checks if a typed expression can be evaluated at compile time.
    pub fn is_compile_time_const(&self, expr: &TypedExpression, ctx: &ScopeContext) -> bool {
        match &expr.kind {
            TypedExpressionKind::Literal(_) => true,
            TypedExpressionKind::Identifier { name, .. } => {
                if let Some(info) = ctx.symbol_table.get(name) {
                    info.is_const
                } else {
                    false
                }
            }
            TypedExpressionKind::BinaryOp(left, _, right) => {
                self.is_compile_time_const(left, ctx) && self.is_compile_time_const(right, ctx)
            }
            TypedExpressionKind::UnaryOp(_, right) => self.is_compile_time_const(right, ctx),
            TypedExpressionKind::TernaryOp(cond, then, else_b) => {
                self.is_compile_time_const(cond, ctx)
                    && self.is_compile_time_const(then, ctx)
                    && self.is_compile_time_const(else_b, ctx)
            }
            TypedExpressionKind::Tuple(elements) => {
                elements.iter().all(|el| self.is_compile_time_const(el, ctx))
            }
            TypedExpressionKind::StructInstantiation(_, args) => {
                args.iter().all(|(_, el)| self.is_compile_time_const(el, ctx))
            }
            TypedExpressionKind::EnumVariantAccess { .. } => true,
            TypedExpressionKind::TaggedUnionInstantiation { payload, .. } => payload
                .as_ref()
                .map_or(true, |p| self.is_compile_time_const(p, ctx)),
            TypedExpressionKind::FunctionCall { .. } => true,
            _ => false,
        }
    }

    /// Converts a `ComptimeValue` from the interpreter back into a `TypedExpressionKind::Literal`
    /// (or equivalent literal-like expression) and its corresponding `JophetType`.
    pub fn comptime_value_to_literal_expr(
        &self,
        value: ComptimeValue,
        span: crate::core::ast::Span,
    ) -> Result<(TypedExpressionKind, JophetType), SemanticError> {
        match value {
            ComptimeValue::Int(i) => Ok((
                TypedExpressionKind::Literal(Literal::Int(i)),
                JophetType::Int(64),
            )),
            ComptimeValue::UInt(u) => {
                if u <= i64::MAX as u64 {
                    Ok((
                        TypedExpressionKind::Literal(Literal::Int(u as i64)),
                        JophetType::UInt(64),
                    ))
                } else {
                    Ok((
                        TypedExpressionKind::UInt64Literal(u),
                        JophetType::UInt(64),
                    ))
                }
            }
            ComptimeValue::Float(f) => Ok((
                TypedExpressionKind::Literal(Literal::Float(f)),
                JophetType::Float(64),
            )),
            ComptimeValue::Bool(b) => Ok((
                TypedExpressionKind::Literal(Literal::Bool(b)),
                JophetType::Bool,
            )),
            ComptimeValue::Char(c) => Ok((
                TypedExpressionKind::Literal(Literal::Char(c)),
                JophetType::Char,
            )),
            ComptimeValue::String(s) => Ok((
                TypedExpressionKind::Literal(Literal::String(s)),
                JophetType::StringSlice,
            )),
            ComptimeValue::Tuple(elements) => {
                let mut typed_elements = Vec::new();
                let mut element_types = Vec::new();
                for el in elements {
                    let (kind, ty) = self.comptime_value_to_literal_expr(el, span.clone())?;
                    typed_elements.push(TypedExpression {
                        kind,
                        jophet_type: ty.clone(),
                        span: span.clone(),
                    });
                    element_types.push(ty);
                }
                Ok((
                    TypedExpressionKind::Tuple(typed_elements),
                    JophetType::Tuple(element_types),
                ))
            }
            ComptimeValue::Struct(name, fields) => {
                let struct_def = self.struct_defs.get(&name).ok_or_else(|| {
                    SemanticError::CtfeError {
                        message: format!(
                            "Internal error: could not find definition for compile-time struct '{}'",
                            name
                        ),
                        span: span.clone(),
                        file_path: self.current_module_path.clone(),
                    }
                })?;

                let mut typed_args = Vec::new();
                for (field_name, _, _, _) in &struct_def.fields {
                    let field_value = fields.get(field_name).ok_or_else(|| {
                        SemanticError::CtfeError {
                            message: format!(
                                "Internal error: compile-time struct '{}' is missing field '{}'",
                                name, field_name
                            ),
                            span: span.clone(),
                            file_path: self.current_module_path.clone(),
                        }
                    })?;
                    let (kind, ty) =
                        self.comptime_value_to_literal_expr(field_value.clone(), span.clone())?;
                    let expr = TypedExpression {
                        kind,
                        jophet_type: ty,
                        span: span.clone(),
                    };
                    typed_args.push((field_name.clone(), expr));
                }

                let struct_type = self.resolve_type(
                    &untyped::Type::Simple(name.clone()),
                    false,
                    None,
                    &ScopeContext::new(),
                    span.clone(),
                )?;

                Ok((
                    TypedExpressionKind::StructInstantiation(name, typed_args),
                    struct_type,
                ))
            }
            ComptimeValue::Array(elements) | ComptimeValue::Vector(elements) => {
                let mut typed_elements = Vec::new();
                let mut member_type = JophetType::Nothing;
                for (i, el) in elements.into_iter().enumerate() {
                    let (kind, ty) = self.comptime_value_to_literal_expr(el, span.clone())?;
                    if i == 0 {
                        member_type = ty.clone();
                    } else if member_type != ty {
                        return Err(SemanticError::CtfeError {
                            message: "Compile-time evaluation resulted in an array with heterogeneous types, which is not supported.".to_string(),
                            span,
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    typed_elements.push(TypedExpression {
                        kind,
                        jophet_type: ty,
                        span: span.clone(),
                    });
                }

                let size = typed_elements.len();
                let final_type = if typed_elements.is_empty() {
                    JophetType::Array {
                        member_type: Box::new(JophetType::Nothing),
                        size: 0,
                    }
                } else {
                    JophetType::Array {
                        member_type: Box::new(member_type),
                        size,
                    }
                };

                Ok((TypedExpressionKind::ArrayLiteral(typed_elements), final_type))
            }
            ComptimeValue::Nothing => Ok((
                TypedExpressionKind::Literal(Literal::Nothing),
                JophetType::Nothing,
            )),
        }
    }
}