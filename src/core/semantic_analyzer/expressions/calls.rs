// src/core/semantic_analyzer/expressions/calls.rs
//! Contains the semantic analysis logic for function and method call expressions.
//!
//! This module handles the resolution of callable entities, argument type checking,
//! and the analysis of built-in functions like `clone` and `println`. It also
//! contains the logic for resolving method calls on concrete types, imported types,
//! and generic types constrained by trait bounds. It now supports utility methods
//! on primitive types like `String` and `Char`, and can analyze calls to closure variables.
//! It enforces move semantics for arguments of owned types, requiring an explicit `clone()`
//! to pass a copy. All built-in fallible functions now return structured, typed errors
//! instead of simple strings. All analysis functions now collect errors in a vector.

use super::{ScopeContext, SemanticAnalyzer};
use crate::core::ast::typed::*;
use crate::core::ast::untyped;
use crate::core::ast::Literal;
use crate::core::ctfe;
use crate::core::semantic_analyzer::types::jophet_type_to_user_string;
use crate::diagnostics::errors::SemanticError;
use std::collections::HashMap;
use std::path::PathBuf;

impl SemanticAnalyzer<'_> {
    /// Translates common cross-platform shell commands.
    ///
    /// This function is called by the analyzer for `command()` calls. It inspects the command
    /// string and, if it recognizes a common but non-portable command (like `ls` on Windows),
    /// it rewrites the command string to its platform-native equivalent. This makes common
    /// scripting tasks portable at the language level.
    fn translate_portable_command(&self, original_cmd: &str) -> String {
        let mut parts = original_cmd.splitn(2, ' ');
        let cmd = parts.next().unwrap_or("");
        let args = parts.next().unwrap_or("");

        let translated_cmd = if cfg!(windows) {
            match cmd {
                "ls" => "dir",
                "rm" => "del",
                "cp" => "copy",
                "mv" => "move",
                "grep" => "findstr",
                "cat" => "type",
                _ => cmd, // Not a command we translate, pass it through
            }
        } else {
            // On non-windows platforms, we can translate common windows commands
            // to their POSIX equivalents if desired.
            match cmd {
                "dir" => "ls",
                "del" => "rm",
                "copy" => "cp",
                "move" => "mv",
                _ => cmd,
            }
        };

        if args.is_empty() {
            translated_cmd.to_string()
        } else {
            format!("{} {}", translated_cmd, args)
        }
    }

    /// Analyzes a function call requested for compile-time evaluation (`const my_func(...)`).
    ///
    /// This function performs a full semantic analysis of the function call, including type checking
    /// and monomorphization, just like a regular runtime call. It then immediately invokes the
    /// CTFE interpreter to execute the call. If successful, it replaces the `const` call
    /// expression node in the AST with the resulting `Literal` value.
    pub fn analyze_const_call_expr(
        &mut self,
        name: &str,
        generic_args: &[untyped::Type],
        args: &[untyped::Expression],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        // Step 1: Analyze this as a regular function call first to get the typed arguments,
        // the mangled name, and the return type.
        let preliminary_call = self.analyze_function_call_expr(name, generic_args, args, ctx, span.clone(), errors)?;
        if preliminary_call.jophet_type == JophetType::ErrorSentinel {
            return Ok(preliminary_call); // Propagate analysis errors
        }

        // Step 2: Deconstruct the analyzed call to build a temporary `ConstCall` node for evaluation.
        let (typed_call_kind, typed_args) = if let TypedExpressionKind::FunctionCall { kind, args } = preliminary_call.kind {
            (kind, args)
        } else {
            // This happens for built-ins like `typeof` that resolve directly. They are already constant.
            return Ok(preliminary_call);
        };

        let const_call_expr = TypedExpression {
            kind: TypedExpressionKind::ConstCall {
                kind: typed_call_kind,
                args: typed_args,
            },
            jophet_type: preliminary_call.jophet_type,
            span: span.clone(),
        };

        // Step 3: Immediately try to evaluate this ConstCall node.
        match self.try_evaluate_at_compile_time(&const_call_expr, ctx, errors) {
            Ok(comptime_value) => {
                // Evaluation succeeded! Replace the initializer with the resulting literal.
                let (new_kind, new_type) = self.comptime_value_to_literal_expr(comptime_value, span.clone())?;
                
                Ok(TypedExpression {
                    kind: new_kind,
                    jophet_type: new_type,
                    span,
                })
            },
            Err(e) => {
                // Evaluation failed. This is a hard error because a const value was required.
                Err(SemanticError::CtfeError {
                    message: format!("Failed to evaluate `const` call to '{}' at compile time: {}", name, e),
                    span,
                    file_path: self.current_module_path.clone(),
                })
            }
        }
    }

    /// Analyzes a function call. It has special handling for built-in functions like
    /// `clone`, `typeof`, `whereis`, `format`, `print`, `println`, `input`, `parse`, `collect`, `command`, `includeC`, `importPy`, a suite
    /// of mathematical functions, and the new `slice` function for Python FFI. For other functions, it checks that the called entity is a
    /// function and that arguments match, enforcing move semantics for owned types. If the function is generic,
    /// it triggers the monomorphization process. It can also now analyze calls to variables of a `Closure` type.
    pub fn analyze_function_call_expr(
        &mut self,
        name: &str,
        generic_args: &[untyped::Type],
        args: &[untyped::Expression],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        // --- BUILT-IN UNSAFE FUNCTIONS ---
        if name == "allocate" || name == "deallocate" {
            if !ctx.in_allow_block {
                return Err(SemanticError::MemoryError {
                    message: format!(
                        "Call to unsafe function `{}` must be inside an `allow` block.",
                        name
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            if name == "allocate" {
                if args.len() != 1 {
                    return Err(SemanticError::TypeError {
                        message: "The `allocate` function expects exactly one argument (size)."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let typed_arg =
                    self.analyze_expression(&args[0], ctx, Some(&JophetType::UInt(64)), errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                if !matches!(typed_arg.jophet_type, JophetType::UInt(_)) {
                    return Err(SemanticError::TypeError { message: format!("Argument to `allocate` must be an unsigned integer, but found '{}'.", jophet_type_to_user_string(&typed_arg.jophet_type)), span: args[0].span.clone(), file_path: self.current_module_path.clone() });
                }
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::FunctionCall {
                        kind: TypedCallKind::Named("jophet_allocate".to_string()),
                        args: vec![typed_arg],
                    },
                    jophet_type: JophetType::RawPointer(Box::new(JophetType::Nothing)),
                    span,
                });
            }
            if name == "deallocate" {
                if args.len() != 1 {
                    return Err(SemanticError::TypeError {
                        message: "The `deallocate` function expects exactly one argument (pointer)."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let typed_arg = self.analyze_expression(&args[0], ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                if !matches!(typed_arg.jophet_type, JophetType::RawPointer(_)) {
                    return Err(SemanticError::TypeError { message: format!("Argument to `deallocate` must be a raw pointer, but found '{}'.", jophet_type_to_user_string(&typed_arg.jophet_type)), span: args[0].span.clone(), file_path: self.current_module_path.clone() });
                }
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::FunctionCall {
                        kind: TypedCallKind::Named("jophet_deallocate".to_string()),
                        args: vec![typed_arg],
                    },
                    jophet_type: JophetType::Nothing,
                    span,
                });
            }
        }

        // --- BUILT-IN NON-MATH FUNCTIONS ---

        if name == "slice" {
            if args.len() != 2 {
                return Err(SemanticError::TypeError {
                    message: "The `slice` function expects exactly two arguments (start, end)."
                        .to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            let mut typed_args = Vec::new();
            for arg in args {
                let typed_arg = self.analyze_expression(arg, ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                // Allow integer types or Nothing (for omitted bounds)
                if !matches!(
                    typed_arg.jophet_type,
                    JophetType::Int(_) | JophetType::UInt(_) | JophetType::Nothing
                ) {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Arguments to `slice` must be integers or omitted, but found '{}'.",
                            jophet_type_to_user_string(&typed_arg.jophet_type)
                        ),
                        span: arg.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
                typed_args.push(typed_arg);
            }

            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named("slice".to_string()),
                    args: typed_args,
                },
                jophet_type: JophetType::PythonSlice,
                span,
            });
        }

        if name == "includeC" {
            if !ctx.in_allow_block {
                return Err(SemanticError::MemoryError {
                    message:
                        "Calling `includeC` is an unsafe operation and must be inside an `allow` block."
                            .to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            if args.len() != 1 {
                return Err(SemanticError::TypeError {
                    message: "The `includeC` function expects exactly one string literal argument."
                        .to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            if let untyped::ExpressionKind::Literal(untyped::Literal::String(header)) = &args[0].kind
            {
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::IncludeC {
                        header: header.clone(),
                    },
                    jophet_type: JophetType::CLibrary {
                        header: PathBuf::from(header),
                    },
                    span,
                });
            } else {
                return Err(SemanticError::TypeError {
                    message: "The argument to `includeC` must be a string literal.".to_string(),
                    span: args[0].span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        if name == "importPy" {
            if args.len() != 1 {
                return Err(SemanticError::TypeError {
                    message:
                        "The `importPy` function expects exactly one string literal argument."
                            .to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            if let untyped::ExpressionKind::Literal(untyped::Literal::String(module_name)) =
                &args[0].kind
            {
                self.needs_python_runtime = true; // Signal that the Python runtime is needed
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::ImportPy {
                        module_name: module_name.clone(),
                    },
                    jophet_type: JophetType::Fallible {
                        ok: Box::new(JophetType::PythonModule),
                        err: Box::new(JophetType::Error {
                            name: "FfiError".to_string(),
                            module_path: PathBuf::from("std"),
                        }),
                    },
                    span,
                });
            } else {
                return Err(SemanticError::TypeError {
                    message: "The argument to `importPy` must be a string literal.".to_string(),
                    span: args[0].span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        if name == "command" {
            if args.is_empty() {
                return Err(SemanticError::TypeError {
                    message: "The `command` function expects at least one argument.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            let mut typed_args = Vec::new();
            for arg in args {
                // The arguments to command MUST be string literals for translation to be possible.
                // This is a reasonable constraint for this feature.
                if let untyped::ExpressionKind::Literal(Literal::String(cmd_str)) = &arg.kind {
                    let translated_cmd_str = self.translate_portable_command(cmd_str);

                    // Create a new literal expression with the translated string
                    let translated_literal_expr = untyped::Expression {
                        kind: untyped::ExpressionKind::Literal(Literal::String(
                            translated_cmd_str,
                        )),
                        span: arg.span.clone(),
                    };

                    // Analyze this new, translated literal as a String
                    let typed_arg = self.analyze_expression(
                        &translated_literal_expr,
                        ctx,
                        Some(&JophetType::String),
                        errors,
                    );
                    if typed_arg.jophet_type == JophetType::ErrorSentinel {
                        return Ok(typed_arg);
                    }
                    typed_args.push(typed_arg);
                } else {
                    // If the argument is not a literal, we cannot translate it.
                    // We will still analyze it and require it to be a String.
                    let typed_arg =
                        self.analyze_expression(arg, ctx, Some(&JophetType::String), errors);
                    if typed_arg.jophet_type == JophetType::ErrorSentinel {
                        return Ok(typed_arg);
                    }
                    if typed_arg.jophet_type != JophetType::String {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "All arguments to `command` must be of type String, but found '{}'. Only string literals will be auto-translated for portability.",
                                jophet_type_to_user_string(&typed_arg.jophet_type)
                            ),
                            span: arg.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    typed_args.push(typed_arg);
                }
            }

            // The return type is now Fallible<Int32, CommandError>
            let return_type = JophetType::Fallible {
                ok: Box::new(JophetType::Int(32)),
                err: Box::new(JophetType::Error {
                    name: "CommandError".to_string(),
                    module_path: PathBuf::from("std"),
                }),
            };

            return Ok(TypedExpression {
                // Use a special mangled name that the expression compiler will handle.
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named("__jophet_variadic_command".to_string()),
                    args: typed_args,
                },
                jophet_type: return_type,
                span,
            });
        }

        if name == "collect" {
            if args.len() != 2 && args.len() != 3 {
                return Err(SemanticError::TypeError {
                    message: "The `collect` function expects 2 or 3 arguments.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            let mut typed_args = Vec::new();
            for arg in args {
                let typed_arg = self.analyze_expression(arg, ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                typed_args.push(typed_arg);
            }

            let element_type = typed_args[0].jophet_type.clone();
            if !matches!(element_type, JophetType::Int(_) | JophetType::Float(_)) {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "The `collect` function only supports Int and Float types, but found '{}'.",
                        jophet_type_to_user_string(&element_type)
                    ),
                    span: args[0].span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            for (i, arg) in typed_args.iter().enumerate().skip(1) {
                if arg.jophet_type != element_type {
                    return Err(SemanticError::TypeError {
                        message: format!("Mismatched types in `collect` arguments. Argument 0 has type '{}', but argument {} has type '{}'.", jophet_type_to_user_string(&element_type), i, jophet_type_to_user_string(&arg.jophet_type)),
                        span: args[i].span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
            }

            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named("jophet_collect".to_string()),
                    args: typed_args,
                },
                jophet_type: JophetType::Vector(Box::new(element_type)),
                span,
            });
        }

        if name == "input" {
            if args.len() > 1 {
                return Err(SemanticError::TypeError {
                    message: "The `input` function expects 0 or 1 arguments.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            let mut typed_args = Vec::new();
            if let Some(prompt_arg) = args.get(0) {
                let typed_prompt = self.analyze_expression(prompt_arg, ctx, None, errors);
                if typed_prompt.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_prompt);
                }
                if !matches!(
                    typed_prompt.jophet_type,
                    JophetType::String | JophetType::StringSlice
                ) {
                    return Err(SemanticError::TypeError {
                        message: "The argument to `input` must be a String or StringSlice."
                            .to_string(),
                        span: prompt_arg.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
                typed_args.push(typed_prompt);
            }
            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named("input".to_string()),
                    args: typed_args,
                },
                jophet_type: JophetType::String,
                span,
            });
        }

        if name == "clone" {
            if args.len() != 1 {
                return Err(SemanticError::TypeError {
                    message: "The `clone` function expects exactly one argument.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            let typed_arg = self.analyze_expression(&args[0], ctx, None, errors);
            if typed_arg.jophet_type == JophetType::ErrorSentinel {
                return Ok(typed_arg);
            }

            if let untyped::ExpressionKind::Identifier(arg_name) = &args[0].kind {
                ctx.moved_vars.remove(arg_name);
            }

            if !self.is_cloneable(&typed_arg.jophet_type) {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Type `{}` is not cloneable.",
                        jophet_type_to_user_string(&typed_arg.jophet_type)
                    ),
                    span: args[0].span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
            return Ok(TypedExpression {
                kind: TypedExpressionKind::Clone(Box::new(typed_arg.clone())),
                jophet_type: typed_arg.jophet_type,
                span,
            });
        }

        if name == "format" {
            if args.len() != 1 {
                return Err(SemanticError::TypeError {
                    message: "The `format` function expects exactly one string literal argument."
                        .to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            if let untyped::ExpressionKind::Literal(Literal::String(content)) = &args[0].kind {
                let temp_parser =
                    crate::core::parser::Parser::new(vec![], self.current_module_path.clone());
                let parts = temp_parser
                    .parse_interpolated_string_content(content, args[0].span.clone())
                    .map_err(|e| SemanticError::from((e, self.current_module_path.clone())))?;
                return self.analyze_interpolated_string_expr(&parts, ctx, span, errors);
            } else {
                return Err(SemanticError::TypeError {
                    message: "The argument to `format` must be a string literal.".to_string(),
                    span: args[0].span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        if name == "whereis" {
            if args.len() != 1 {
                return Err(SemanticError::TypeError {
                    message: "The `whereis` function expects exactly one argument.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            let var_name = if let untyped::ExpressionKind::Identifier(name) = &args[0].kind {
                name
            } else {
                return Err(SemanticError::TypeError {
                    message: "The argument to `whereis` must be a variable name.".to_string(),
                    span: args[0].span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            };

            let location_str = if ctx.ownership_map.contains_key(var_name) {
                "heap"
            } else {
                "stack"
            };

            return Ok(TypedExpression {
                kind: TypedExpressionKind::Literal(Literal::String(location_str.to_string())),
                jophet_type: JophetType::StringSlice,
                span,
            });
        }

        if name == "typeof" {
            if args.len() != 1 {
                return Err(SemanticError::TypeError {
                    message: "The `typeof` function expects exactly one argument.".to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            let typed_arg = self.analyze_expression(&args[0], ctx, None, errors);
            if typed_arg.jophet_type == JophetType::ErrorSentinel {
                return Ok(typed_arg);
            }
            let type_str = jophet_type_to_user_string(&typed_arg.jophet_type);

            return Ok(TypedExpression {
                kind: TypedExpressionKind::Literal(Literal::String(type_str)),
                jophet_type: JophetType::StringSlice,
                span,
            });
        }

        if name == "println" || name == "print" {
            let mut args_typed = Vec::new();
            for arg in args {
                let typed_arg = self.analyze_expression(arg, ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                if !self.is_printable(&typed_arg.jophet_type) {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Type '{}' is not printable and cannot be passed to '{}'.",
                            jophet_type_to_user_string(&typed_arg.jophet_type),
                            name
                        ),
                        span: arg.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
                args_typed.push(typed_arg);
            }
            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named(name.to_string()),
                    args: args_typed,
                },
                jophet_type: JophetType::Nothing,
                span,
            });
        }

        // --- BUILT-IN MATH FUNCTIONS ---

        if name == "minimum" || name == "maximum" {
            let is_max = name == "maximum";

            // Case 1: Two numeric arguments (non-fallible)
            if args.len() == 2 {
                let mut typed_args = Vec::new();
                for arg in args {
                    let typed_arg =
                        self.analyze_expression(arg, ctx, Some(&JophetType::Float(64)), errors);
                    if typed_arg.jophet_type == JophetType::ErrorSentinel {
                        return Ok(typed_arg);
                    }
                    typed_args.push(typed_arg);
                }

                if typed_args[0].jophet_type == JophetType::Float(64)
                    && typed_args[1].jophet_type == JophetType::Float(64)
                {
                    let mangled_name = if is_max {
                        "fmax".to_string()
                    } else {
                        "fmin".to_string()
                    };
                    return Ok(TypedExpression {
                        kind: TypedExpressionKind::FunctionCall {
                            kind: TypedCallKind::Named(mangled_name),
                            args: typed_args,
                        },
                        jophet_type: JophetType::Float(64),
                        span,
                    });
                }

                return Err(SemanticError::TypeError {
                    message: format!(
                        "The two-argument version of `{}` requires both arguments to be of type Float64.",
                        name
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            // Case 2: One collection argument (panics on empty)
            if args.len() == 1 {
                let typed_arg = self.analyze_expression(&args[0], ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }

                let (mangled_name, return_type) = match &typed_arg.jophet_type {
                    JophetType::Array { member_type, .. } | JophetType::Vector(member_type) => {
                        if matches!(member_type.as_ref(), JophetType::PythonObject { .. }) {
                            self.needs_python_runtime = true;
                            (
                                if is_max {
                                    "__jophet_python_maximum_or_panic"
                                } else {
                                    "__jophet_python_minimum_or_panic"
                                }
                                .to_string(),
                                JophetType::PythonObject {
                                    brand: Box::new(self.py_any_brand.clone()),
                                },
                            )
                        } else if !matches!(
                            member_type.as_ref(),
                            JophetType::Int(_)
                                | JophetType::UInt(_)
                                | JophetType::Float(_)
                                | JophetType::Char
                        ) {
                            return Err(SemanticError::TypeError {
                                message: format!(
                                    "Cannot find the {} of a collection with non-orderable element type '{}'.",
                                    name,
                                    jophet_type_to_user_string(member_type)
                                ),
                                span: args[0].span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        } else {
                            (
                                if is_max {
                                    "__jophet_collection_maximum_or_panic"
                                } else {
                                    "__jophet_collection_minimum_or_panic"
                                }
                                .to_string(),
                                *member_type.clone(),
                            )
                        }
                    }
                    JophetType::String | JophetType::StringSlice => (
                        if is_max {
                            "__jophet_string_maximum_or_panic"
                        } else {
                            "__jophet_string_minimum_or_panic"
                        }
                        .to_string(),
                        JophetType::Char,
                    ),
                    JophetType::PythonObject { .. } => {
                        self.needs_python_runtime = true;
                        (
                            if is_max {
                                "__jophet_python_maximum_or_panic"
                            } else {
                                "__jophet_python_minimum_or_panic"
                            }
                            .to_string(),
                            JophetType::PythonObject {
                                brand: Box::new(self.py_any_brand.clone()),
                            },
                        )
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "The one-argument version of `{}` expects a collection (Array, Vector, String, PythonObject), but found type '{}'.",
                                name,
                                jophet_type_to_user_string(&typed_arg.jophet_type)
                            ),
                            span: args[0].span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                };

                return Ok(TypedExpression {
                    kind: TypedExpressionKind::FunctionCall {
                        kind: TypedCallKind::Named(mangled_name),
                        args: vec![typed_arg],
                    },
                    jophet_type: return_type,
                    span,
                });
            }

            // If we reach here, the number of arguments is wrong.
            return Err(SemanticError::TypeError {
                message: format!(
                    "The `{}` function expects either one collection argument or two numeric arguments.",
                    name
                ),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        let c_name: Option<String> = match name {
            "sqrt" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "exp" | "log2"
            | "floor" | "ceil" | "round" | "trunc" | "deg2rad" | "rad2deg" => Some(name.to_string()),
            "ln" => Some("log".to_string()),
            "log10" => Some("log10".to_string()),
            "abs" => Some("fabs".to_string()),
            _ => None,
        };

        if let Some(mangled_name) = c_name {
            let expected_arg_count = 1;

            if args.len() != expected_arg_count {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Math function `{}` expects {} argument(s), but found {}.",
                        name,
                        expected_arg_count,
                        args.len()
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            let mut typed_args = Vec::new();
            for arg in args {
                let typed_arg =
                    self.analyze_expression(arg, ctx, Some(&JophetType::Float(64)), errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                if typed_arg.jophet_type != JophetType::Float(64) {
                    // This could be extended to allow Float32 and insert a cast, but for now we'll be strict.
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Arguments to math function `{}` must be of type Float64, but found {}.",
                            name,
                            jophet_type_to_user_string(&typed_arg.jophet_type)
                        ),
                        span: arg.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
                typed_args.push(typed_arg);
            }

            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named(mangled_name),
                    args: typed_args,
                },
                jophet_type: JophetType::Float(64),
                span,
            });
        }

        // --- USER-DEFINED FUNCTIONS AND CLOSURES ---

        if self.generic_functions.contains_key(name) {
            return self.monomorphize_function_call(name, generic_args, args, ctx, span, errors);
        }

        if let Some(info) = ctx.symbol_table.get(name).cloned() {
            let (expected_params, ret_type, call_kind) = match &info.jophet_type {
                JophetType::Function { params, ret } => (
                    params.clone(),
                    *ret.clone(),
                    TypedCallKind::Named(
                        info.mangled_name
                            .clone()
                            .unwrap_or_else(|| name.to_string()),
                    ),
                ),
                JophetType::Closure { params, ret, .. } => {
                    let callable_expr = self.analyze_identifier_expr(name, ctx, span.clone())?;
                    (
                        params.clone(),
                        *ret.clone(),
                        TypedCallKind::Closure {
                            callable_expr: Box::new(callable_expr),
                            params: params.clone(),
                            ret: ret.clone(),
                        },
                    )
                }
                _ => {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "'{}' is not a function or closure and cannot be called",
                            name
                        ),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
            };

            let mut typed_args = Vec::new();
            for (i, arg_expr) in args.iter().enumerate() {
                let expected_arg_type = expected_params.get(i);
                let typed_arg = self.analyze_expression(arg_expr, ctx, expected_arg_type, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }

                // If the parameter expects an owned type and isn't a borrow, it's a move.
                if self.is_owned_type(&typed_arg.jophet_type)
                    && !matches!(
                        expected_arg_type,
                        Some(JophetType::Reference(_)) | Some(JophetType::MutableReference(_))
                    )
                {
                    if let TypedExpressionKind::Identifier {
                        name: source_name, ..
                    } = &typed_arg.kind
                    {
                        if ctx.ownership_map.contains_key(source_name) {
                            ctx.moved_vars.insert(source_name.clone());
                        }
                    }
                }
                typed_args.push(typed_arg);
            }

            self.check_arguments(
                name,
                &mut typed_args,
                &expected_params,
                false,
                span.clone(),
            )?;

            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: call_kind,
                    args: typed_args,
                },
                jophet_type: ret_type,
                span,
            });
        }

        Err(SemanticError::NameError {
            message: format!("Undefined function '{}'", name),
            span: span.clone(),
            file_path: self.current_module_path.clone(),
        })
    }

    /// Analyzes a method call, resolving the method name based on the object's type.
    /// It can resolve methods on concrete types, imported types (from metadata), and generic types
    /// via their trait bounds. It has special handling for built-in methods (like `flatten`) and enforces
    /// mutability rules for mutating methods. It now enforces move semantics for owned arguments.
    pub fn analyze_method_call_expr(
        &mut self,
        object: &untyped::Expression,
        method_name: &str,
        args: &[untyped::Expression],
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_object = self.analyze_expression(object, ctx, None, errors);
        if typed_object.jophet_type == JophetType::ErrorSentinel {
            return Ok(typed_object);
        }
        let object_type = typed_object.jophet_type.clone();

        // --- NEW FFI METHOD CALL HANDLING ---
        if let JophetType::CLibrary { .. } = &object_type {
            if !ctx.in_allow_block {
                return Err(SemanticError::MemoryError {
                    message:
                        "Calling an external C function is an unsafe operation and must be inside an `allow` block."
                            .to_string(),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }
            // This is a C FFI call. We don't know the signature, so we analyze arguments
            // without hints and assume the return type is `void` for now. The backend will
            // handle this dynamically. More advanced FFI could allow type hints here.
            let mut typed_args = Vec::new();
            for arg in args {
                let typed_arg = self.analyze_expression(arg, ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                typed_args.push(typed_arg);
            }
            return Ok(TypedExpression {
                kind: TypedExpressionKind::MethodCall {
                    object: Box::new(typed_object),
                    mangled_name: method_name.to_string(), // The C function name
                    args: typed_args,
                },
                // We can't know the return type. Defaulting to `Nothing` is safest.
                // The C backend will simply not use the return value unless it's assigned.
                jophet_type: JophetType::Nothing,
                span,
            });
        }

        if let JophetType::PythonModule | JophetType::PythonObject { .. } = &object_type {
            // Handle the special `.length()` method for Python objects by re-typing it.
            if method_name == "length" {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message:
                            "The `.length()` method for Python objects does not take any arguments."
                                .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: "length".to_string(),
                        args: vec![],
                    },
                    jophet_type: JophetType::UInt(64), // The result is a native integer
                    span,
                });
            }

            if method_name == "flatten" {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message:
                            "The `.flatten()` method for Python objects does not take any arguments."
                                .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                self.needs_python_runtime = true;
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: "flatten".to_string(),
                        args: vec![],
                    },
                    jophet_type: JophetType::PythonObject {
                        brand: Box::new(self.py_any_brand.clone()),
                    },
                    span,
                });
            }

            // Python FFI call. This is also dynamic.
            self.needs_python_runtime = true;
            let mut typed_args = Vec::new();
            for arg in args {
                let typed_arg = self.analyze_expression(arg, ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                typed_args.push(typed_arg);
            }
            return Ok(TypedExpression {
                kind: TypedExpressionKind::MethodCall {
                    object: Box::new(typed_object),
                    mangled_name: method_name.to_string(), // The Python function name
                    args: typed_args,
                },
                jophet_type: JophetType::PythonObject {
                    brand: Box::new(self.py_any_brand.clone()),
                },
                span,
            });
        }
        // --- END FFI METHOD CALL HANDLING ---

        // Identify mutating methods and check receiver mutability.
        let is_mutating_method = matches!(method_name, "push" | "set" | "pop" | "mutateEach");

        if is_mutating_method {
            if let untyped::ExpressionKind::Identifier(name) = &object.kind {
                if let Some(info) = ctx.symbol_table.get(name) {
                    if !info.is_mutable {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Cannot call mutating method `{}` on immutable variable `{}`. Help: declare `{}` as `mutable`.",
                                method_name, name, name
                            ),
                            span: object.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                }
            }
        }

        // Handle Char built-in methods
        if let JophetType::Char = &object_type {
            let (mangled_name, num_args, ret_type) = match method_name {
                "isAlphanumeric" => ("jophet_char_is_alphanumeric", 0, JophetType::Bool),
                "isAlphabetic" => ("jophet_char_is_alphabetic", 0, JophetType::Bool),
                "isDigit" => ("jophet_char_is_digit", 0, JophetType::Bool),
                "isWhitespace" => ("jophet_char_is_whitespace", 0, JophetType::Bool),
                _ => {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Type 'Char' does not have a method named '{}'.",
                            method_name
                        ),
                        span,
                        file_path: self.current_module_path.clone(),
                    })
                }
            };

            if args.len() != num_args {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Method '{}' expects {} arguments, but {} were provided.",
                        method_name,
                        num_args,
                        args.len()
                    ),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            return Ok(TypedExpression {
                kind: TypedExpressionKind::MethodCall {
                    object: Box::new(typed_object),
                    mangled_name: mangled_name.to_string(),
                    args: vec![],
                },
                jophet_type: ret_type,
                span,
            });
        }

        // Handle built-in methods for collections.
        match method_name {
            "unchecked" => {
                if !ctx.in_allow_block {
                    return Err(SemanticError::MemoryError {
                        message: "Call to unsafe method `unchecked` must be inside an `allow` block.".to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                if args.len() != 1 {
                    return Err(SemanticError::TypeError { message: "The `.unchecked()` method expects exactly one argument (index).".to_string(), span, file_path: self.current_module_path.clone() });
                }
                let typed_index =
                    self.analyze_expression(&args[0], ctx, Some(&JophetType::UInt(64)), errors);
                if typed_index.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_index);
                }
                let member_type = match &object_type {
                    JophetType::Array { member_type, .. }
                    | JophetType::Vector(member_type) => member_type.as_ref().clone(),
                    _ => return Err(SemanticError::TypeError { message: format!("Type '{}' does not have a method named 'unchecked'.", jophet_type_to_user_string(&object_type)), span, file_path: self.current_module_path.clone() })
                };
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: "unchecked".to_string(),
                        args: vec![typed_index],
                    },
                    jophet_type: member_type,
                    span,
                });
            }
            "flatten" => {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message: "The `.flatten()` method does not take any arguments."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }

                let inner_member_type = match &object_type {
                    JophetType::Vector(outer_member) => {
                        if let JophetType::Vector(inner_member) = outer_member.as_ref() {
                            inner_member.as_ref().clone()
                        } else {
                            return Err(SemanticError::TypeError {
                                message: format!("Cannot flatten a non-nested vector. Expected Vector<Vector<T>>, but found '{}'.", jophet_type_to_user_string(&object_type)),
                                span: object.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                    }
                    JophetType::Array {
                        member_type: outer_member,
                        ..
                    } => {
                        if let JophetType::Array {
                            member_type: inner_member,
                            ..
                        } = outer_member.as_ref()
                        {
                            inner_member.as_ref().clone()
                        } else {
                            return Err(SemanticError::TypeError {
                                message: format!("Cannot flatten a non-nested array. Expected Array<Array<T, M>, N>, but found '{}'.", jophet_type_to_user_string(&object_type)),
                                span: object.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!("The `.flatten()` method can only be called on nested collections (Vector<Vector<T>> or Array<Array<T, M>, N>), but found '{}'.", jophet_type_to_user_string(&object_type)),
                            span: object.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                };

                let result_type = JophetType::Vector(Box::new(inner_member_type));

                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: "flatten".to_string(), // Special name for the backend
                        args: vec![],
                    },
                    jophet_type: result_type,
                    span,
                });
            }
            "map" => {
                if args.len() != 1 {
                    return Err(SemanticError::TypeError {
                        message: "The `.map()` method expects exactly one closure argument."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }

                let typed_closure = self.analyze_expression(&args[0], ctx, None, errors);
                if typed_closure.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_closure);
                }

                let (element_type, closure_params, closure_ret_type) =
                    match (&object_type, &typed_closure.jophet_type) {
                        (JophetType::Vector(el), JophetType::Closure { params, ret, .. }) => {
                            (el.as_ref(), params, ret.as_ref())
                        }
                        (
                            JophetType::Array { member_type, .. },
                            JophetType::Closure { params, ret, .. },
                        ) => (member_type.as_ref(), params, ret.as_ref()),
                        _ => {
                            return Err(SemanticError::TypeError {
                                message: format!(
                                    "The `.map()` method can only be called on an Array or Vector, and its argument must be a closure. Found object of type '{}' and argument of type '{}'.",
                                    jophet_type_to_user_string(&object_type),
                                    jophet_type_to_user_string(&typed_closure.jophet_type)
                                ),
                                span,
                                file_path: self.current_module_path.clone(),
                            });
                        }
                    };

                if closure_params.len() != 1
                    || !self.is_type_compatible(&closure_params[0], element_type)
                {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Closure signature mismatch for `.map()`. Expected a closure that takes one argument of type '{}', but found a closure that takes ({})",
                            jophet_type_to_user_string(element_type),
                            closure_params.iter().map(jophet_type_to_user_string).collect::<Vec<_>>().join(", ")
                        ),
                        span: args[0].span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }

                // The result of mapping a Vector or an Array is always a new Vector.
                let result_vector_type = JophetType::Vector(Box::new(closure_ret_type.clone()));

                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: "map".to_string(), // Special name for the backend
                        args: vec![typed_closure],
                    },
                    jophet_type: result_vector_type,
                    span,
                });
            }
            "isEmpty" => {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message: "The `.isEmpty()` method does not take any arguments."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let mangled_name = match &object_type {
                    JophetType::Vector(_) => "Vector_isEmpty",
                    JophetType::String | JophetType::StringSlice => "String_isEmpty",
                    JophetType::Array { size, .. } => {
                        return Ok(TypedExpression {
                            kind: TypedExpressionKind::Literal(Literal::Bool(*size == 0)),
                            jophet_type: JophetType::Bool,
                            span,
                        });
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a method named 'isEmpty'.",
                                jophet_type_to_user_string(&object_type)
                            ),
                            span,
                            file_path: self.current_module_path.clone(),
                        })
                    }
                };
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: mangled_name.to_string(),
                        args: vec![],
                    },
                    jophet_type: JophetType::Bool,
                    span,
                });
            }
            "pop" => {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message: "The `.pop()` method does not take any arguments.".to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let (mangled_name, ret_type) = match &object_type {
                    JophetType::Vector(m) => (
                        "Vector_pop",
                        JophetType::Fallible {
                            ok: m.clone(),
                            err: Box::new(JophetType::Nothing),
                        },
                    ),
                    JophetType::String => (
                        "String_pop",
                        JophetType::Fallible {
                            ok: Box::new(JophetType::Char),
                            err: Box::new(JophetType::Nothing),
                        },
                    ),
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a method named 'pop'.",
                                jophet_type_to_user_string(&object_type)
                            ),
                            span,
                            file_path: self.current_module_path.clone(),
                        })
                    }
                };
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: mangled_name.to_string(),
                        args: vec![],
                    },
                    jophet_type: ret_type,
                    span,
                });
            }
            "first" | "last" => {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "The `.{}` method does not take any arguments.",
                            method_name
                        ),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let (mangled_name, ret_type) = match &object_type {
                    JophetType::Vector(m) => (
                        if method_name == "first" {
                            "Vector_first"
                        } else {
                            "Vector_last"
                        },
                        JophetType::Fallible {
                            ok: m.clone(),
                            err: Box::new(JophetType::Nothing),
                        },
                    ),
                    JophetType::String => (
                        if method_name == "first" {
                            "String_first"
                        } else {
                            "String_last"
                        },
                        JophetType::Fallible {
                            ok: Box::new(JophetType::Char),
                            err: Box::new(JophetType::Nothing),
                        },
                    ),
                    JophetType::Array { member_type, size } => {
                        if *size == 0 {
                            return Ok(TypedExpression {
                                kind: TypedExpressionKind::FallibleWrap {
                                    is_ok: false,
                                    expr: Box::new(TypedExpression {
                                        kind: TypedExpressionKind::Literal(Literal::Nothing),
                                        jophet_type: JophetType::Nothing,
                                        span: span.clone(),
                                    }),
                                },
                                jophet_type: JophetType::Fallible {
                                    ok: member_type.clone(),
                                    err: Box::new(JophetType::Nothing),
                                },
                                span,
                            });
                        }
                        let index = if method_name == "first" { 0 } else { size - 1 };
                        let index_expr = TypedExpression {
                            kind: TypedExpressionKind::Literal(Literal::Int(index as i64)),
                            jophet_type: JophetType::Int(64),
                            span: span.clone(),
                        };
                        let array_index_expr = TypedExpression {
                            kind: TypedExpressionKind::ArrayIndex {
                                array: Box::new(typed_object),
                                index: Box::new(index_expr),
                                size: Some(*size),
                            },
                            jophet_type: (**member_type).clone(),
                            span: span.clone(),
                        };
                        return Ok(TypedExpression {
                            kind: TypedExpressionKind::FallibleWrap {
                                is_ok: true,
                                expr: Box::new(array_index_expr),
                            },
                            jophet_type: JophetType::Fallible {
                                ok: member_type.clone(),
                                err: Box::new(JophetType::Nothing),
                            },
                            span,
                        });
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a method named '{}'.",
                                jophet_type_to_user_string(&object_type),
                                method_name
                            ),
                            span,
                            file_path: self.current_module_path.clone(),
                        })
                    }
                };
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: mangled_name.to_string(),
                        args: vec![],
                    },
                    jophet_type: ret_type,
                    span,
                });
            }
            "contains" => {
                if args.len() != 1 {
                    return Err(SemanticError::TypeError {
                        message: "The `.contains()` method expects exactly one argument."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let (mangled_name, expected_arg_type) = match &object_type {
                    JophetType::Vector(m) => ("Vector_contains", m.as_ref().clone()),
                    JophetType::String | JophetType::StringSlice => {
                        ("String_contains", JophetType::String)
                    }
                    JophetType::Array { member_type, .. } => {
                        ("Array_contains", member_type.as_ref().clone())
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a method named 'contains'.",
                                jophet_type_to_user_string(&object_type)
                            ),
                            span,
                            file_path: self.current_module_path.clone(),
                        })
                    }
                };
                let typed_arg =
                    self.analyze_expression(&args[0], ctx, Some(&expected_arg_type), errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: mangled_name.to_string(),
                        args: vec![typed_arg],
                    },
                    jophet_type: JophetType::Bool,
                    span,
                });
            }
            "eachIndex" | "mutateEach" => {
                if args.len() != 1 {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "The `.{}` method expects exactly one closure argument.",
                            method_name
                        ),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                // Full analysis happens in `analyze_statement`, we just validate the call shape here.
                let typed_arg = self.analyze_expression(&args[0], ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                if !matches!(typed_arg.jophet_type, JophetType::Closure { .. }) {
                    return Err(SemanticError::TypeError {
                        message: format!("Argument to `.{}` must be a closure.", method_name),
                        span: args[0].span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: method_name.to_string(),
                        args: vec![typed_arg],
                    },
                    jophet_type: JophetType::Nothing,
                    span,
                });
            }
            "get" => {
                if let JophetType::String = &object_type {
                    if args.len() != 1 {
                        return Err(SemanticError::TypeError {
                            message:
                                "The `String.get()` method expects exactly one argument (the index)."
                                    .to_string(),
                            span,
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    let typed_index =
                        self.analyze_expression(&args[0], ctx, Some(&JophetType::UInt(64)), errors);
                    if typed_index.jophet_type == JophetType::ErrorSentinel {
                        return Ok(typed_index);
                    }
                    if !matches!(typed_index.jophet_type, JophetType::UInt(_)) {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "String index must be an unsigned integer, but found '{}'.",
                                jophet_type_to_user_string(&typed_index.jophet_type)
                            ),
                            span: args[0].span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    return Ok(TypedExpression {
                        kind: TypedExpressionKind::MethodCall {
                            object: Box::new(typed_object),
                            mangled_name: "String_get".to_string(),
                            args: vec![typed_index],
                        },
                        jophet_type: JophetType::Fallible {
                            ok: Box::new(JophetType::Char),
                            err: Box::new(JophetType::Nothing),
                        },
                        span,
                    });
                } else if matches!(&object_type, JophetType::Dictionary { .. }) {
                    if args.len() != 1 {
                        return Err(SemanticError::TypeError {
                            message:
                                "The `Dictionary.get()` method expects exactly one argument (key)."
                                    .to_string(),
                            span,
                            file_path: self.current_module_path.clone(),
                        });
                    }

                    if let JophetType::Dictionary {
                        key: key_ty,
                        value: value_ty,
                    } = &object_type
                    {
                        let typed_key = self.analyze_expression(&args[0], ctx, Some(key_ty), errors);
                        if typed_key.jophet_type == JophetType::ErrorSentinel {
                            return Ok(typed_key);
                        }

                        if !self.is_type_compatible(&typed_key.jophet_type, key_ty) {
                            return Err(SemanticError::TypeError { message: format!("Mismatched key type in 'get'. Expected '{}', found '{}'.", jophet_type_to_user_string(key_ty), jophet_type_to_user_string(&typed_key.jophet_type)), span: args[0].span.clone(), file_path: self.current_module_path.clone() });
                        }

                        return Ok(TypedExpression {
                            kind: TypedExpressionKind::MethodCall {
                                object: Box::new(typed_object),
                                mangled_name: "get".to_string(),
                                args: vec![typed_key],
                            },
                            jophet_type: JophetType::Fallible {
                                ok: value_ty.clone(),
                                err: Box::new(JophetType::Nothing),
                            },
                            span,
                        });
                    } else {
                        unreachable!()
                    }
                } else {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Type '{}' does not have a `.get()` method.",
                            jophet_type_to_user_string(&object_type)
                        ),
                        span: object.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
            }
            "length" => {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message: "The `.length()` method does not take any arguments.".to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let (jophet_type, kind) = match &object_type {
                    JophetType::Array { size, .. } => (
                        JophetType::UInt(64),
                        TypedExpressionKind::Literal(Literal::Int(*size as i64)),
                    ),
                    JophetType::Dictionary { .. }
                    | JophetType::Vector(_)
                    | JophetType::String
                    | JophetType::StringSlice => (
                        JophetType::UInt(64),
                        TypedExpressionKind::MethodCall {
                            object: Box::new(typed_object),
                            mangled_name: "length".to_string(),
                            args: vec![],
                        },
                    ),
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a `.length()` method.",
                                jophet_type_to_user_string(&object_type)
                            ),
                            span: object.span.clone(),
                            file_path: self.current_module_path.clone(),
                        })
                    }
                };

                return Ok(TypedExpression {
                    kind,
                    jophet_type,
                    span,
                });
            }
            "set" => {
                if !matches!(&object_type, JophetType::Dictionary { .. }) {
                    return Err(SemanticError::TypeError {
                        message: format!(
                            "Type '{}' does not have a `.set()` method.",
                            jophet_type_to_user_string(&object_type)
                        ),
                        span: object.span.clone(),
                        file_path: self.current_module_path.clone(),
                    });
                }
                if args.len() != 2 {
                    return Err(SemanticError::TypeError {
                        message: "The `.set()` method expects exactly two arguments (key, value)."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }

                if let JophetType::Dictionary {
                    key: key_ty,
                    value: value_ty,
                } = &object_type
                {
                    let typed_key = self.analyze_expression(&args[0], ctx, Some(key_ty), errors);
                    if typed_key.jophet_type == JophetType::ErrorSentinel {
                        return Ok(typed_key);
                    }
                    let typed_value =
                        self.analyze_expression(&args[1], ctx, Some(value_ty), errors);
                    if typed_value.jophet_type == JophetType::ErrorSentinel {
                        return Ok(typed_value);
                    }

                    if !self.is_type_compatible(&typed_key.jophet_type, key_ty) {
                        return Err(SemanticError::TypeError { message: format!("Mismatched key type in 'set'. Expected '{}', found '{}'.", jophet_type_to_user_string(key_ty), jophet_type_to_user_string(&typed_key.jophet_type)), span: args[0].span.clone(), file_path: self.current_module_path.clone() });
                    }
                    if !self.is_type_compatible(&typed_value.jophet_type, value_ty) {
                        return Err(SemanticError::TypeError { message: format!("Mismatched value type in 'set'. Expected '{}', found '{}'.", jophet_type_to_user_string(value_ty), jophet_type_to_user_string(&typed_value.jophet_type)), span: args[1].span.clone(), file_path: self.current_module_path.clone() });
                    }

                    return Ok(TypedExpression {
                        kind: TypedExpressionKind::MethodCall {
                            object: Box::new(typed_object),
                            mangled_name: "set".to_string(),
                            args: vec![typed_key, typed_value],
                        },
                        jophet_type: JophetType::Nothing,
                        span,
                    });
                } else {
                    unreachable!()
                }
            }
            "push" => {
                if args.len() != 1 {
                    return Err(SemanticError::TypeError {
                        message: "The `.push()` method expects exactly one argument.".to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                let mut typed_arg = self.analyze_expression(&args[0], ctx, None, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }

                match &object_type {
                    JophetType::Vector(member_type) => {
                        // Auto-convert StringSlice literal to String for Vector<String>.push()
                        if *member_type.as_ref() == JophetType::String
                            && typed_arg.jophet_type == JophetType::StringSlice
                        {
                            let arg_span = typed_arg.span.clone();
                            typed_arg = TypedExpression {
                                kind: TypedExpressionKind::New {
                                    jophet_type: JophetType::String,
                                    args: vec![typed_arg],
                                },
                                jophet_type: JophetType::String,
                                span: arg_span,
                            };
                        }

                        if !self.is_type_compatible(&typed_arg.jophet_type, member_type) {
                            return Err(SemanticError::TypeError {
                                message: format!(
                                    "Invalid argument for `Vector.push`. Expected {}, found {}.",
                                    jophet_type_to_user_string(member_type),
                                    jophet_type_to_user_string(&typed_arg.jophet_type)
                                ),
                                span: args[0].span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                    }
                    JophetType::String => {
                        if !matches!(
                            &typed_arg.jophet_type,
                            JophetType::Char | JophetType::String | JophetType::StringSlice
                        ) {
                            return Err(SemanticError::TypeError { message: format!("Invalid argument for `String.push`. Expected Char, String, or StringSlice, but found {}.", jophet_type_to_user_string(&typed_arg.jophet_type)), span: args[0].span.clone(), file_path: self.current_module_path.clone() });
                        }
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a `.push()` method.",
                                jophet_type_to_user_string(&object_type)
                            ),
                            span: object.span.clone(),
                            file_path: self.current_module_path.clone(),
                        })
                    }
                }

                return Ok(TypedExpression {
                    kind: TypedExpressionKind::MethodCall {
                        object: Box::new(typed_object),
                        mangled_name: "push".to_string(),
                        args: vec![typed_arg],
                    },
                    jophet_type: JophetType::Nothing,
                    span,
                });
            }
            "characters" => {
                if !args.is_empty() {
                    return Err(SemanticError::TypeError {
                        message: "The `.characters()` method does not take any arguments."
                            .to_string(),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }
                match &object_type {
                    JophetType::String | JophetType::StringSlice => {
                        // The mangled name must match the C runtime function name.
                        let mangled_name = "String_characters".to_string();

                        // The receiver is the object itself. The backend will handle taking
                        // its address for the C runtime function call.
                        return Ok(TypedExpression {
                            kind: TypedExpressionKind::MethodCall {
                                object: Box::new(typed_object),
                                mangled_name,
                                args: vec![],
                            },
                            jophet_type: JophetType::Vector(Box::new(JophetType::Char)),
                            span,
                        });
                    }
                    _ => {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "Type '{}' does not have a `.characters()` method.",
                                jophet_type_to_user_string(&object_type)
                            ),
                            span: object.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                }
            }
            _ => {} // Fall through for user-defined methods.
        }

        // Handle module-level function calls like `my_module.my_func()`
        if let JophetType::Module { name: mod_name } = &object_type {
            let (expected_params, ret_type, mangled_name) = {
                let module_scope =
                    self.modules.get(mod_name).ok_or_else(|| SemanticError::InternalError {
                        message: format!(
                            "Module '{}' imported but not found in analyzer state.",
                            mod_name
                        ),
                        span: object.span.clone(),
                        file_path: self.current_module_path.clone(),
                    })?;
                let symbol_info = module_scope
                    .symbol_table
                    .get(method_name)
                    .ok_or_else(|| SemanticError::NameError {
                        message: format!(
                            "Function '{}' not found in module '{}'",
                            method_name, mod_name
                        ),
                        span: span.clone(),
                        file_path: self.current_module_path.clone(),
                    })?;
                let (expected_params, ret_type) =
                    if let JophetType::Function { params, ret } = &symbol_info.jophet_type {
                        (params.clone(), *ret.clone())
                    } else {
                        return Err(SemanticError::TypeError {
                            message: format!(
                                "'{}' in module '{}' is not a function",
                                method_name, mod_name
                            ),
                            span: span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    };
                let mangled_name =
                    symbol_info
                        .mangled_name
                        .as_ref()
                        .ok_or_else(|| SemanticError::InternalError {
                            message: format!(
                                "Imported function '{}' has no mangled name",
                                method_name
                            ),
                            span: span.clone(),
                            file_path: self.current_module_path.clone(),
                        })?
                        .clone();
                (expected_params, ret_type, mangled_name)
            };

            let mut typed_args: Vec<_> = Vec::new();
            for (i, arg) in args.iter().enumerate() {
                let expected_arg_type = expected_params.get(i);
                let typed_arg = self.analyze_expression(arg, ctx, expected_arg_type, errors);
                if typed_arg.jophet_type == JophetType::ErrorSentinel {
                    return Ok(typed_arg);
                }
                typed_args.push(typed_arg);
            }

            self.check_arguments(
                method_name,
                &mut typed_args,
                &expected_params,
                false,
                span.clone(),
            )?;

            return Ok(TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named(mangled_name),
                    args: typed_args,
                },
                jophet_type: ret_type,
                span,
            });
        }

        // Centralized method lookup for all other types (structs, primitives, generics).
        let symbol_info = self
            .find_method_for_type(&object_type, method_name, ctx, span.clone())?
            .ok_or_else(|| {
                SemanticError::TypeError {
                    message: format!(
                        "No method named '{}' found for type '{}'",
                        method_name,
                        jophet_type_to_user_string(&object_type)
                    ),
                    span: span.clone(),
                    file_path: self.current_module_path.clone(),
                }
            })?;

        let mangled_name = symbol_info.mangled_name.unwrap(); // Should always exist for methods
        let (expected_params, return_type) =
            if let JophetType::Function { params, ret } = symbol_info.jophet_type {
                (params, *ret)
            } else {
                unreachable!("Method symbol info was not a function type");
            };

        let mut typed_args: Vec<_> = Vec::new();
        for (i, arg_expr) in args.iter().enumerate() {
            // +1 to skip 'self' parameter
            let expected_arg_type = expected_params.get(i + 1);
            let typed_arg = self.analyze_expression(arg_expr, ctx, expected_arg_type, errors);
            if typed_arg.jophet_type == JophetType::ErrorSentinel {
                return Ok(typed_arg);
            }

            // Handle move semantics for method arguments
            if self.is_owned_type(&typed_arg.jophet_type) {
                if let TypedExpressionKind::Identifier {
                    name: source_name, ..
                } = &typed_arg.kind
                {
                    if ctx.ownership_map.contains_key(source_name) {
                        ctx.moved_vars.insert(source_name.clone());
                    }
                }
            }
            typed_args.push(typed_arg);
        }

        self.check_arguments(
            method_name,
            &mut typed_args,
            &expected_params,
            true,
            span.clone(),
        )?;

        Ok(TypedExpression {
            kind: TypedExpressionKind::MethodCall {
                object: Box::new(typed_object),
                mangled_name,
                args: typed_args,
            },
            jophet_type: return_type,
            span,
        })
    }
}