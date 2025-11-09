// src/backend/c/statements.rs
//! Handles the compilation of Jophet statements into their C equivalents.
//!
//! This module is responsible for translating the `TypedStatement` AST nodes into
//! C statements. It covers control flow (if, while, for), variable declarations,
//! assignments, returns, and other statement-level constructs, including memory
//! management via `delete`.

use super::expressions::CExpression;
use super::Generator;
use crate::core::ast::typed::*;
use crate::core::ast::Span;
use std::fmt::Write;

impl Generator {
    /// Compiles a statement that is inside a function, applying indentation.
    ///
    /// # Panics
    /// Panics if writing the initial tab character to the output buffer fails.
    pub fn compile_statement_in_function(&mut self, stmt: &TypedStatement) {
        write!(&mut self.output, "\t").unwrap();
        self.compile_statement_common(stmt, false);
    }

    /// Compiles a statement that is in the top-level `main` scope, applying indentation.
    ///
    /// # Panics
    /// Panics if writing the initial tab character to the output buffer fails.
    pub fn compile_statement_in_main(&mut self, stmt: &TypedStatement) {
        write!(&mut self.output, "\t").unwrap();
        self.compile_statement_common(stmt, true);
    }

    /// Generates a full C statement for a cleanup call for a given type and variable name.
    /// The returned string is a complete statement, including a semicolon, or a block for complex types.
    ///
    /// This function will also set the `runtime_needed` flag if the type requires
    /// a runtime function for cleanup (e.g., `String_delete`). It now correctly generates
    /// cleanup logic for `Tuple`, `Array`, `TaggedUnion`, and `Error` types containing owned data.
    ///
    /// # Arguments
    /// * `jophet_type` - The type of the variable to be cleaned up.
    /// * `c_name` - The C variable name or expression to access the variable.
    /// * `is_pointer` - `true` if `c_name` refers to a pointer that should be passed directly
    ///   to functions like `free`. `false` if it's a value whose address should be taken.
    pub(super) fn get_cleanup_call(
        &mut self,
        jophet_type: &JophetType,
        c_name: &str,
        is_pointer: bool,
    ) -> String {
        match jophet_type {
            JophetType::String => {
                self.runtime_needed = true;
                let arg = if is_pointer {
                    c_name.to_string()
                } else {
                    format!("&{}", c_name)
                };
                format!("String_delete({});", arg)
            }
            JophetType::Vector(member_type) => {
                self.runtime_needed = true;
                if self.type_needs_cleanup(member_type) {
                    let helper_name = self.get_or_create_vector_deep_delete_helper(member_type);
                    let arg = if is_pointer {
                        c_name.to_string()
                    } else {
                        format!("&{}", c_name)
                    };
                    format!("{}({});", helper_name, arg)
                } else {
                    let arg = if is_pointer {
                        c_name.to_string()
                    } else {
                        format!("&{}", c_name)
                    };
                    format!("Vector_delete({});", arg)
                }
            }
            JophetType::Closure { .. } => {
                self.runtime_needed = true;
                let arg = if is_pointer {
                    c_name.to_string()
                } else {
                    format!("&{}", c_name)
                };
                format!("JophetClosure_delete({});", arg)
            }
            JophetType::Pointer(inner) if matches!(**inner, JophetType::Struct { .. }) => {
                format!("free({});", c_name)
            }
            JophetType::Struct { name, .. } => {
                if self.structs_with_destructors.contains(name) {
                    let arg = if is_pointer {
                        c_name.to_string()
                    } else {
                        format!("&{}", c_name)
                    };
                    format!("{}_delete({});", name, arg)
                } else {
                    "".to_string()
                }
            }
            JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } => {
                if self.tagged_unions_with_destructors.contains(name) {
                    let arg = if is_pointer {
                        c_name.to_string()
                    } else {
                        format!("&{}", c_name)
                    };
                    format!("{}_delete({});", name, arg)
                } else {
                    "".to_string()
                }
            }
            JophetType::PythonModule | JophetType::PythonObject { .. } => {
                self.python_runtime_needed = true;
                format!("jophet_py_decref({});", c_name)
            }
            JophetType::Tuple(elements) => {
                let mut cleanup_stmts = Vec::new();
                let access_op = if is_pointer { "->" } else { "." };

                for (i, element_type) in elements.iter().enumerate() {
                    if self.type_needs_cleanup(element_type) {
                        let field_expr = format!("{}{}{}", c_name, access_op, format!("f{}", i));
                        let cleanup_call = self.get_cleanup_call(element_type, &field_expr, false);
                        cleanup_stmts.push(cleanup_call);
                    }
                }

                if cleanup_stmts.is_empty() {
                    "".to_string()
                } else {
                    // Return a block of statements
                    format!("{{ {} }}", cleanup_stmts.join(" "))
                }
            }
            JophetType::Array { member_type, size } => {
                if self.type_needs_cleanup(member_type) {
                    let mut output = String::new();
                    let loop_var = format!("__del_idx_{}", self.temp_var_counter);
                    self.temp_var_counter += 1;

                    writeln!(&mut output, "{{").unwrap();
                    writeln!(
                        &mut output,
                        "\tfor (size_t {} = 0; {} < {}; ++{}) {{",
                        loop_var, loop_var, size, loop_var
                    )
                    .unwrap();

                    let element_expr = format!("{}[{}]", c_name, loop_var);
                    let cleanup_call = self.get_cleanup_call(member_type, &element_expr, false);

                    writeln!(&mut output, "\t\t{};", cleanup_call).unwrap();
                    writeln!(&mut output, "\t}}").unwrap();
                    writeln!(&mut output, "}}").unwrap();
                    output
                } else {
                    "".to_string()
                }
            }
            _ => "".to_string(),
        }
    }

    /// Checks if a Jophet type requires any cleanup action.
    /// This function will also set the `runtime_needed` flag if the type requires
    /// a runtime function for cleanup.
    pub(super) fn type_needs_cleanup(&mut self, jophet_type: &JophetType) -> bool {
        match jophet_type {
            JophetType::String
            | JophetType::Vector(_)
            | JophetType::Pointer(_)
            | JophetType::Closure { .. } => {
                if matches!(
                    jophet_type,
                    JophetType::String | JophetType::Vector(_) | JophetType::Closure { .. }
                ) {
                    self.runtime_needed = true;
                }
                true
            }
            JophetType::PythonModule | JophetType::PythonObject { .. } => {
                self.python_runtime_needed = true;
                true
            }
            JophetType::Struct { name, .. } => self.structs_with_destructors.contains(name),
            JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } => {
                self.tagged_unions_with_destructors.contains(name)
            }
            JophetType::Tuple(elements) => elements.iter().any(|t| self.type_needs_cleanup(t)),
            JophetType::Array { member_type, .. } => self.type_needs_cleanup(member_type),
            _ => false,
        }
    }

    /// The core compilation logic for all statement types.
    ///
    /// This function matches on the `TypedStatementKind` and dispatches to helper
    /// functions or generates the C code directly. For cleanup statements (`Delete`
    /// and `AutoDelete`), it generates the C code but pushes it onto a cleanup
    /// stack for the current scope instead of emitting it immediately. It now safely
    /// handles moved values for `AutoDelete` by checking if the value is non-NULL.
    /// For `print` and `println` calls, it now correctly handles memory for temporary
    /// r-value arguments to prevent leaks. It also distinguishes between executable and
    /// library builds for variable declarations.
    /// It now handles `return` statements for arrays by generating a struct copy.
    ///
    /// # Arguments
    /// * `stmt` - The untyped statement to analyze.
    /// * `is_main_scope` - A flag indicating if the statement is in the top-level `main` function.
    ///
    /// # Panics
    /// Panics if any `writeln!` macro fails when writing to the internal output buffer.
    pub(super) fn compile_statement_common(&mut self, stmt: &TypedStatement, is_main_scope: bool) {
        match &stmt.kind {
            TypedStatementKind::Delete(name, jophet_type) => {
                let is_pointer = matches!(
                    jophet_type,
                    JophetType::Pointer(_) | JophetType::PythonModule | JophetType::PythonObject { .. }
                );
                let sanitized_name = self.sanitize_c_keyword(name);
                let cleanup_code = self.get_cleanup_call(jophet_type, &sanitized_name, is_pointer);
                if !cleanup_code.is_empty() {
                    // Manual delete is emitted immediately, but we also zero out the variable
                    // to prevent a double-delete if it's in a scope that gets an auto-delete.
                    let c_type = self.jophet_type_to_c_string(jophet_type);
                    writeln!(
                        &mut self.output,
                        "{{\n\t\t{};\n\t\tmemset(&{}, 0, sizeof({}));\n\t}}",
                        cleanup_code, sanitized_name, c_type
                    )
                    .unwrap();
                }
            }
            TypedStatementKind::AutoDelete(name, jophet_type) => {
                let is_pointer = matches!(
                    jophet_type,
                    JophetType::Pointer(_) | JophetType::PythonModule | JophetType::PythonObject { .. }
                );
                let sanitized_name = self.sanitize_c_keyword(name);
                let cleanup_code = self.get_cleanup_call(jophet_type, &sanitized_name, is_pointer);
                if !cleanup_code.is_empty() {
                    // For auto-deletes, we add a check. If a value was moved, its struct will be
                    // zeroed out, and its `data` pointer (or the pointer itself) will be NULL, preventing the cleanup.
                    let check_condition = if is_pointer {
                        sanitized_name.clone()
                    } else {
                        format!("{}.data", sanitized_name)
                    };

                    let safe_cleanup = format!("if ({}) {{ {} }}", check_condition, cleanup_code);
                    self.scope_cleanup_stack
                        .last_mut()
                        .unwrap()
                        .push(safe_cleanup);
                }
            }
            TypedStatementKind::If(if_stmt) => self.compile_if_statement(if_stmt),
            TypedStatementKind::While(while_stmt) => self.compile_while_statement(while_stmt),
            TypedStatementKind::For(for_stmt) => self.compile_for_statement(for_stmt),
            TypedStatementKind::ForIn(for_in_stmt) => self.compile_for_in_statement(for_in_stmt),
            TypedStatementKind::Break => {
                writeln!(&mut self.output, "break;").unwrap();
            }
            TypedStatementKind::Continue => {
                writeln!(&mut self.output, "continue;").unwrap();
            }
            // These definition types are handled entirely in the `forward_declare` pass.
            // They do not generate any code in the statement compilation pass.
            TypedStatementKind::StructDef(_)
            | TypedStatementKind::EnumDef(_)
            | TypedStatementKind::UnionDef(_)
            | TypedStatementKind::TaggedUnionDef(_)
            | TypedStatementKind::ErrorDef(_)
            | TypedStatementKind::TraitDef(_)
            | TypedStatementKind::FunctionDecl(_) => {}
            TypedStatementKind::VariableDecl(decl) => {
                let initializer = self.compile_expression(&decl.initializer);
                // For arrays, jophet_type_to_c_string returns the base type.
                // The dimension suffix is added to the variable name.
                let base_c_type = self.jophet_type_to_c_string(&decl.jophet_type);
                let dimension_suffix = self.get_array_dimension_suffix(&decl.jophet_type);
                
                // --- FIX: Handle non-constant initializers for globals ---
                if self.is_lib_build && is_main_scope {
                    let mangled_name = format!("__jophet_global_var_{}", decl.name);
                    
                    // Check if the initializer is a compile-time constant.
                    let is_constant = matches!(decl.initializer.kind, TypedExpressionKind::Literal(_) | TypedExpressionKind::UInt64Literal(_));

                    if is_constant {
                        // It's a constant, so we can initialize it directly as a global.
                        let decl_string = format!(
                            "{} {}{} = {};",
                            base_c_type, mangled_name, dimension_suffix, initializer
                        );
                        writeln!(&mut self.global_defs, "{}", decl_string).unwrap();
                    } else {
                        // It's a dynamic value (function call, etc.).
                        // Declare it without an initializer.
                        let decl_string = format!(
                            "{} {}{};",
                            base_c_type, mangled_name, dimension_suffix
                        );
                        writeln!(&mut self.global_defs, "{}", decl_string).unwrap();
                        
                        // Add the assignment to the library's init function.
                        let assignment_string = format!(
                            "\t{} = {};",
                            mangled_name, initializer
                        );
                        writeln!(&mut self.library_init_body, "{}", assignment_string).unwrap();
                    }
                } else {
                    // It's a local variable (in `main` or a function).
                    let mutability = if decl.is_const {
                        "const "
                    } else if decl.is_mutable || self.type_needs_cleanup(&decl.jophet_type) {
                        ""
                    } else {
                        "const "
                    };
                    let sanitized_name = self.sanitize_c_keyword(&decl.name);
                    let decl_string = format!(
                        "{}{} {}{} = {};",
                        mutability, base_c_type, sanitized_name, dimension_suffix, initializer
                    );
                    writeln!(&mut self.output, "{}", decl_string).unwrap();
                }
            }
            TypedStatementKind::DestructuringDecl(decl) => {
                self.compile_destructuring_declaration(decl);
            }
            TypedStatementKind::ArrayDestructuringDecl(decl) => {
                self.compile_array_destructuring_declaration(decl, &stmt.span);
            }
            TypedStatementKind::Return(expr) => {
                // Before returning, execute all cleanup actions for the current scope.
                for action in self.scope_cleanup_stack.last().unwrap().iter().rev() {
                    writeln!(&mut self.output, "{}", action).unwrap();
                }
                if expr.jophet_type == JophetType::Nothing {
                    writeln!(&mut self.output, "return;").unwrap();
                } else if matches!(expr.jophet_type, JophetType::Array { .. }) {
                    // FIX: C cannot return arrays directly. We wrap the local array in a
                    // struct and return the struct by value. The C return type is already
                    // a struct, so this will match.
                    let compiled_expr = self.compile_expression(expr);
                    let c_return_type = self.jophet_type_to_c_return_string(&expr.jophet_type);
                    writeln!(&mut self.output, "\t{} __return_val;", c_return_type).unwrap();
                    writeln!(&mut self.output, "\tmemcpy(&__return_val.data, {}, sizeof(__return_val.data));", compiled_expr).unwrap();
                    writeln!(&mut self.output, "\treturn __return_val;").unwrap();
                } else {
                    let compiled_expr = self.compile_expression(expr);
                    writeln!(&mut self.output, "return {};", compiled_expr).unwrap();
                }
            }
            TypedStatementKind::ExpressionStatement(expr) => {
                // Special handling for built-in functions, which are statements in C, not expressions.
                if let TypedExpressionKind::FunctionCall { kind, args } = &expr.kind {
                    if let TypedCallKind::Named(name) = kind {
                        if name == "println" || name == "print" {
                            for (i, arg) in args.iter().enumerate() {
                                if i > 0 {
                                    write!(&mut self.output, "printf(\" \");").unwrap();
                                }
                                
                                // `ensure_lvalue` will create a temporary for r-values and schedule cleanup.
                                let lvalue = self.ensure_lvalue(arg);
                                let print_call = self.get_print_call(&arg.jophet_type, &lvalue, false);
                                write!(&mut self.output, "{}", print_call).unwrap();
                            }
                            if name == "println" {
                                writeln!(&mut self.output, "printf(\"\\n\");").unwrap();
                            }
                            // Explicitly flush stdout to ensure output appears immediately in interactive sessions like the REPL.
                            writeln!(&mut self.output, "fflush(stdout);").unwrap();
                            // This writeln is necessary to ensure a newline after the C statements.
                            writeln!(&mut self.output).unwrap();
                            return; // The statement is fully compiled, no semicolon needed.
                        }
                    }
                }

                // Special handling for `.push()` method calls, which may need to generate statements
                // to handle temporary r-value arguments correctly.
                if let TypedExpressionKind::MethodCall {
                    object,
                    mangled_name,
                    args,
                } = &expr.kind
                {
                    if mangled_name == "push" {
                        self.compile_push_statement(object, args);
                        writeln!(&mut self.output).unwrap();
                        return;
                    }
                }

                // For all other expressions, compile them and add a semicolon.
                let compiled_expr = self.compile_expression(expr);
                if !compiled_expr.is_empty() {
                    writeln!(&mut self.output, "{};", compiled_expr).unwrap();
                }
            }
            TypedStatementKind::Assignment(lvalue, rvalue) => {
                self.compile_assignment(lvalue, rvalue);
            }
            // Yield is handled by the `compile_switch_branch_body` and `compile_catch_expression`
            // logic, which assigns the yielded value to the correct temporary variable. It is
            // not a standalone statement that generates code here.
            TypedStatementKind::Yield(_) => {}
        }
    }

    /// Compiles a `.push` method call as a statement.
    ///
    /// This is handled specially at the statement level to correctly manage ownership and temporary
    /// variables. It now handles `Vector<PythonObject>` by incrementing the reference count of
    /// the pushed object. For other owned types, it generates a clone. It now uses `ensure_lvalue`
    /// to simplify its implementation.
    fn compile_push_statement(&mut self, object: &TypedExpression, args: &[TypedExpression]) {
        self.runtime_needed = true;
        let compiled_obj = self.compile_expression(object);
        let arg = &args[0];
        
        // The argument to push must first be stored in a temporary lvalue variable.
        // `ensure_lvalue` handles this perfectly.
        let arg_lvalue = self.ensure_lvalue(arg);

        let push_call = match &object.jophet_type {
            JophetType::Vector(member_type) => {
                // Handle PythonObject push by incrementing its reference count.
                if matches!(member_type.as_ref(), JophetType::PythonObject { .. }) {
                    self.python_runtime_needed = true;
                    format!(
                        "{{ Py_INCREF({}); Vector_push(&{}, &{}); }}",
                        arg_lvalue, compiled_obj, arg_lvalue
                    )
                } else {
                    let value_to_push = if self.type_is_cloneable(member_type) {
                        self.get_clone_call(member_type, &arg_lvalue)
                    } else {
                        arg_lvalue.clone()
                    };

                    let temp_final_var = format!("__push_final_{}", self.temp_var_counter);
                    self.temp_var_counter += 1;
                    let final_c_type = self.jophet_type_to_c_string(member_type);

                    format!(
                        "{{ {} {} = {}; Vector_push(&{}, &{}); }}",
                        final_c_type, temp_final_var, value_to_push, compiled_obj, temp_final_var
                    )
                }
            }
            JophetType::String => match &arg.jophet_type {
                JophetType::Char => format!("String_builder_append_char(&{}, {});", compiled_obj, arg_lvalue),
                JophetType::String => format!("String_builder_append_string(&{}, &{});", compiled_obj, arg_lvalue),
                JophetType::StringSlice => format!("String_builder_append(&{}, {});", compiled_obj, arg_lvalue),
                _ => unreachable!("Semantic analysis should prevent invalid types for String.push."),
            },
            _ => unreachable!("Semantic analysis should prevent push on non-pushable types."),
        };
        write!(&mut self.output, "{}", push_call).unwrap();
    }

    /// Compiles a block of statements within a new scope. It manages the cleanup stack
    /// to ensure resources created within the block are destroyed before the block exits.
    fn compile_block(&mut self, block: &[TypedStatement]) -> String {
        // A new block is a new scope.
        self.scope_cleanup_stack.push(Vec::new());

        // Temporarily take ownership of the main output buffer to compile the block in isolation.
        let mut block_output = String::new();
        let original_output = std::mem::take(&mut self.output);
        std::mem::swap(&mut self.output, &mut block_output);

        for stmt in block {
            self.compile_statement_common(stmt, false);
        }

        // Add the end-of-scope cleanup actions before the block scope closes.
        let cleanup_actions = self
            .scope_cleanup_stack
            .pop()
            .expect("Cleanup stack should not be empty at end of block");
        for action in cleanup_actions.iter().rev() {
            writeln!(&mut self.output, "{}", action).expect("Failed to write cleanup action");
        }

        // Restore the original output buffer and return the compiled block.
        std::mem::swap(&mut self.output, &mut block_output);
        self.output = original_output;
        block_output
    }

    /// Compiles an `if-else if-else` chain into its C equivalent.
    ///
    /// This function handles both standard boolean conditions and conditional bindings
    /// (e.g., `if val: Type = fallible_expr`). It is designed to correctly handle
    /// several edge cases for conditional bindings:
    /// 1. If the binding target is `_`, no C variable is declared for the unwrapped value.
    /// 2. It correctly handles the special `void*` pointer return from `Dictionary.get`,
    ///    dereferencing it to get the actual value.
    /// 3. It correctly reuses the temporary `Result` variable generated by complex
    ///    fallible expressions to avoid C type mismatches.
    ///
    /// # Panics
    /// Panics if any `write!` or `writeln!` macro fails when writing to the internal output buffer.
    fn compile_if_statement(&mut self, if_stmt: &TypedIfStatement) {
        if let Some((binding_name, _)) = &if_stmt.binding {
            if let JophetType::Fallible { ok, .. } = &if_stmt.condition.jophet_type {
                // --- NEW, MORE ROBUST LOGIC FOR CONDITIONAL BINDING ---
                let compiled_condition = self.compile_expression_internal(&if_stmt.condition);

                // This logic is now similar to `compile_catch_expression` to correctly handle
                // special cases like `Dictionary.get`, which has its own temporary variable.
                let temp_result_var = match compiled_condition {
                    CExpression::Temporary(name) => name,
                    CExpression::Simple(expr_str) => {
                        let name = format!("__if_let_res_{}", self.temp_var_counter);
                        self.temp_var_counter += 1;
                        let result_c_type = self
                            .fallible_type_to_c_result_string(&if_stmt.condition.jophet_type);
                        writeln!(
                            &mut self.output,
                            "{} {} = {};",
                            result_c_type, name, expr_str
                        )
                        .unwrap();
                        name
                    }
                };

                writeln!(&mut self.output, "if ({}.is_ok) {{", temp_result_var).unwrap();

                // The 'then' block needs to be compiled in a new scope where the binding exists.
                let mut then_block_output = String::new();
                let original_output = std::mem::take(&mut self.output);
                std::mem::swap(&mut self.output, &mut then_block_output);

                // --- Compile 'then' block ---
                self.scope_cleanup_stack.push(Vec::new());

                // FIX #1: If the binding is `_`, we don't declare any variable.
                if binding_name != "_" {
                    // 1. Declare the new bound variable.
                    let binding_c_type = self.jophet_type_to_c_string(ok);
                    let sanitized_binding_name = self.sanitize_c_keyword(binding_name);

                    // FIX #2: Special handling for Dictionary.get, which returns a pointer
                    // that needs dereferencing. This mirrors the logic in `catch`.
                    let initializer = if let TypedExpressionKind::MethodCall {
                        mangled_name, ..
                    } = &if_stmt.condition.kind
                    {
                        if mangled_name == "get" {
                            format!("*({}*){}.data.ok", binding_c_type, temp_result_var)
                        } else {
                            format!("{}.data.ok", temp_result_var)
                        }
                    } else {
                        format!("{}.data.ok", temp_result_var)
                    };

                    writeln!(
                        &mut self.output,
                        "\tconst {} {} = {};",
                        binding_c_type, sanitized_binding_name, initializer
                    )
                    .unwrap();
                }

                // 2. Compile the rest of the statements in the block.
                for stmt in &if_stmt.then_block {
                    self.compile_statement_in_function(stmt);
                }

                // 3. Add cleanup for the scope.
                let cleanup_actions = self
                    .scope_cleanup_stack
                    .pop()
                    .expect("Cleanup stack should be present");
                for action in cleanup_actions.iter().rev() {
                    writeln!(&mut self.output, "\t{}", action).unwrap();
                }

                // --- Restore buffers ---
                std::mem::swap(&mut self.output, &mut then_block_output);
                self.output = original_output;

                // Indent and write the compiled block.
                for line in then_block_output.lines() {
                    writeln!(&mut self.output, "\t{}", line).unwrap();
                }
            } else {
                unreachable!("Semantic analyzer should ensure binding implies fallible type");
            }
        } else {
            // --- EXISTING LOGIC FOR STANDARD `if` ---
            let compiled_condition = self.compile_expression(&if_stmt.condition);
            writeln!(&mut self.output, "if ({}) {{", compiled_condition).unwrap();
            let then_block_str = self.compile_block(&if_stmt.then_block);
            for line in then_block_str.lines() {
                writeln!(&mut self.output, "\t{}", line).unwrap();
            }
        }

        // --- COMMON LOGIC for `else` and closing `}` ---
        write!(&mut self.output, "}}").unwrap();

        if let Some(else_block) = &if_stmt.else_block {
            write!(&mut self.output, " else ").unwrap();
            match else_block.as_ref() {
                TypedElseBlock::ElseIf(next_if) => {
                    self.compile_if_statement(next_if);
                }
                TypedElseBlock::Else(else_stmts) => {
                    writeln!(&mut self.output, "{{").unwrap();
                    let else_block_str = self.compile_block(else_stmts);
                    for line in else_block_str.lines() {
                        writeln!(&mut self.output, "\t{}", line).unwrap();
                    }
                    writeln!(&mut self.output, "}}").unwrap();
                }
            }
        } else {
            writeln!(&mut self.output).unwrap();
        }
    }

    /// Compiles a `while` loop into its C equivalent.
    ///
    /// # Panics
    /// Panics if any `writeln!` macro fails when writing to the internal output buffer.
    fn compile_while_statement(&mut self, while_stmt: &TypedWhileStatement) {
        let condition_str = self.compile_expression(&while_stmt.condition);
        writeln!(&mut self.output, "while ({}) {{", condition_str).unwrap();

        // Compile the loop body and indent it.
        let body_str = self.compile_block(&while_stmt.body);
        for line in body_str.lines() {
            writeln!(&mut self.output, "\t{}", line).unwrap();
        }

        writeln!(&mut self.output, "}}").unwrap();
    }

    /// Compiles a Jophet numeric range `for` loop into a C `for` loop.
    ///
    /// This is a complex translation because Jophet's `for` loop supports iterating
    /// both upwards and downwards with a dynamic step, determined at runtime.
    /// The generated C code does the following:
    /// 1. Creates temporary variables to hold the start, stop, and step values, evaluating them once.
    /// 2. Determines the direction of the loop (incrementing or decrementing) at runtime.
    /// 3. Uses a C `for` loop with a condition and increment/decrement step that respects the determined direction.
    /// 4. Wraps the entire construct in a block `{ ... }` to scope the temporary variables.
    ///
    /// # Panics
    /// Panics if any `writeln!` macro fails when writing to the internal output buffer.
    fn compile_for_statement(&mut self, for_stmt: &TypedForStatement) {
        let start_str = self.compile_expression(&for_stmt.start);
        let stop_str = self.compile_expression(&for_stmt.stop);
        let step_str = for_stmt
            .step
            .as_ref()
            .map_or("1".to_string(), |s| self.compile_expression(s));
        let iterator_type = self.jophet_type_to_c_string(&for_stmt.iterator_type);
        let iterator_name = self.sanitize_c_keyword(&for_stmt.iterator_name);

        // Create temporary variables to avoid re-evaluating expressions in the loop header.
        let temp_start_var = format!("__jophet_for_start_{}", self.temp_var_counter);
        let temp_stop_var = format!("__jophet_for_stop_{}", self.temp_var_counter);
        let temp_step_var = format!("__jophet_for_step_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        writeln!(&mut self.output, "{{").unwrap();
        writeln!(
            &mut self.output,
            "\t{} {} = {};",
            iterator_type, temp_start_var, start_str
        )
        .unwrap();
        writeln!(
            &mut self.output,
            "\t{} {} = {};",
            iterator_type, temp_stop_var, stop_str
        )
        .unwrap();
        writeln!(
            &mut self.output,
            "\t{} {} = {};",
            iterator_type, temp_step_var, step_str
        )
        .unwrap();

        // This boolean expression determines if we are counting up or down.
        let condition_check = format!("{} <= {}", temp_start_var, temp_stop_var);
        // The loop condition changes based on the direction.
        let loop_condition = format!(
            "({}) ? ({} <= {}) : ({} >= {})",
            condition_check, iterator_name, temp_stop_var, iterator_name, temp_stop_var
        );
        // The increment step adds or subtracts the step value based on direction.
        let loop_increment = format!(
            "{} += ({}) ? {} : -{}",
            iterator_name, condition_check, temp_step_var, temp_step_var
        );

        writeln!(
            &mut self.output,
            "\tfor ({} {} = {}; {}; {}) {{",
            iterator_type, iterator_name, temp_start_var, loop_condition, loop_increment
        )
        .unwrap();

        // Compile the loop body and indent it.
        let body_str = self.compile_block(&for_stmt.body);
        for line in body_str.lines() {
            writeln!(&mut self.output, "\t\t{}", line).unwrap();
        }

        writeln!(&mut self.output, "\t}}").unwrap();
        writeln!(&mut self.output, "}}").unwrap();
    }

    /// Compiles a Jophet `for-in` loop into a C `for` loop over an index. It now correctly
    /// handles nested loops over Python objects by capturing the unique counter ID for each loop
    /// before its body is compiled, preventing state corruption.
    fn compile_for_in_statement(&mut self, for_stmt: &TypedForInStatement) {
        let collection_str = self.compile_expression(&for_stmt.collection);
        let iterator_c_type = self.jophet_type_to_c_string(&for_stmt.iterator_type);
        let iterator_name = self.sanitize_c_keyword(&for_stmt.iterator_name);
        
        let line = self.source_map.line_for_byte(for_stmt.collection.span.start);
        
        writeln!(&mut self.output, "{{").unwrap(); // Open a scope for the whole for-in construct

        let mut py_loop_counter_id: Option<usize> = None;

        match &for_stmt.collection.jophet_type {
            JophetType::Array { size, .. } => {
                let index_var = format!("__jophet_for_idx_{}", self.temp_var_counter);
                self.temp_var_counter += 1;
                let bounds_check_fn = self.get_bounds_check_helper();

                writeln!(
                    &mut self.output,
                    "\tfor (size_t {} = 0; {} < {}; ++{}) {{",
                    index_var, index_var, size, index_var
                )
                .unwrap();
                writeln!(&mut self.output, "\t\tconst {} {} = {}[{}({}, {}, \"{}\", {})];", iterator_c_type, iterator_name, collection_str, bounds_check_fn, index_var, size, self.source_map.filename(), line).unwrap();
            }
            JophetType::Vector(member_type) => {
                let index_var = format!("__jophet_for_idx_{}", self.temp_var_counter);
                self.temp_var_counter += 1;
                let bounds_check_fn = self.get_bounds_check_helper();
                let member_c_type = self.jophet_type_to_c_string(member_type);

                writeln!(
                    &mut self.output,
                    "\tfor (size_t {} = 0; {} < {}.len; ++{}) {{",
                    index_var, index_var, collection_str, index_var
                )
                .unwrap();
                writeln!(
                    &mut self.output,
                    "\t\tconst {} {} = (({}*){}.data)[{}({}, {}.len, \"{}\", {})];",
                    iterator_c_type, iterator_name, member_c_type, collection_str, bounds_check_fn, index_var, collection_str, self.source_map.filename(), line
                )
                .unwrap();
            }
            JophetType::String => {
                let index_var = format!("__jophet_for_idx_{}", self.temp_var_counter);
                self.temp_var_counter += 1;
                let bounds_check_fn = self.get_bounds_check_helper();

                writeln!(
                    &mut self.output,
                    "\tfor (size_t {} = 0; {} < {}.len; ++{}) {{",
                    index_var, index_var, collection_str, index_var
                )
                .unwrap();
                writeln!(
                    &mut self.output,
                    "\t\tconst char {} = {}.data[{}({}, {}.len, \"{}\", {})];",
                    iterator_name, collection_str, bounds_check_fn, index_var, collection_str, self.source_map.filename(), line
                )
                .unwrap();
            }
            JophetType::PythonObject { .. } => {
                self.python_runtime_needed = true;
                let counter_id = self.temp_var_counter;
                self.temp_var_counter += 1;
                py_loop_counter_id = Some(counter_id);

                let iter_var = format!("__py_iter_{}", counter_id);
                let item_var = format!("__py_item_{}", counter_id);

                writeln!(&mut self.output, "\tPythonObject {} = PyObject_GetIter({});", iter_var, collection_str).unwrap();
                writeln!(&mut self.output, "\tif ({} == NULL) {{", iter_var).unwrap();
                writeln!(&mut self.output, "\t\tJophetString err_msg = get_python_exception_string();").unwrap();
                writeln!(&mut self.output, "\t\tjophet_panic_on_py_err(&err_msg, \"{}\", {});", self.source_map.filename(), line).unwrap();
                writeln!(&mut self.output, "\t}}").unwrap();
                writeln!(&mut self.output, "\tPythonObject {};", item_var).unwrap();
                writeln!(&mut self.output, "\twhile (({} = PyIter_Next({}))) {{", item_var, iter_var).unwrap();
                writeln!(&mut self.output, "\t\t{} {} = {};", iterator_c_type, iterator_name, item_var).unwrap();
            }
             _ => unreachable!("Semantic analysis should prevent non-iterables here."),
        }

        // Compile the loop body.
        let mut temp_output = String::new();
        std::mem::swap(&mut self.output, &mut temp_output);
        let body_str = self.compile_block(&for_stmt.body);
        std::mem::swap(&mut self.output, &mut temp_output);

        for line in body_str.lines() {
            writeln!(&mut self.output, "\t\t{}", line).unwrap();
        }
        
        // Add cleanup and closing braces.
        if let Some(counter_id) = py_loop_counter_id {
            // It's a Python loop.
            let item_var = format!("__py_item_{}", counter_id);
            writeln!(&mut self.output, "\t\tjophet_py_decref({});", item_var).unwrap();
            
            writeln!(&mut self.output, "\t}}").unwrap(); // Close while loop
            
            let iter_var = format!("__py_iter_{}", counter_id);
            writeln!(&mut self.output, "\tjophet_py_decref({});", iter_var).unwrap();
        } else {
            // It's a native loop.
            writeln!(&mut self.output, "\t}}").unwrap(); // Close for loop's block
        }
        
        writeln!(&mut self.output, "}}").unwrap(); // Close the outer scope
    }

    /// Compiles a destructuring declaration statement for tuples and structs.
    ///
    /// This function handles both positional and labeled destructuring.
    ///
    /// Example (Tuple): `(x: Int64, y: String) = my_tuple` becomes:
    /// ```c
    /// Tuple_Int64_String __temp_destructure_0 = my_tuple;
    /// const int64_t x = __temp_destructure_0.f0;
    /// const JophetString y = __temp_destructure_0.f1;
    /// ```
    fn compile_destructuring_declaration(&mut self, decl: &DestructuringDecl) {
        let initializer = self.compile_expression(&decl.initializer);
        let temp_var = format!("__temp_destructure_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        let initializer_c_type = self.jophet_type_to_c_string(&decl.initializer.jophet_type);
        writeln!(
            &mut self.output,
            "{} {} = {};",
            initializer_c_type, temp_var, initializer
        )
        .unwrap();

        let is_pointer = matches!(&decl.initializer.jophet_type, JophetType::Pointer(_));
        let access_op = if is_pointer { "->" } else { "." };

        let base_type = if let JophetType::Pointer(inner) = &decl.initializer.jophet_type {
            inner.as_ref()
        } else {
            &decl.initializer.jophet_type
        };

        match base_type {
            JophetType::Tuple(_) => {
                for (i, target) in decl.targets.iter().enumerate() {
                    if target.var_name == "_" || target.var_name == ".." {
                        continue;
                    }
                    let mutability =
                        if target.is_mutable || self.type_needs_cleanup(&target.jophet_type) {
                            ""
                        } else {
                            "const "
                        };
                    let base_c_type = self.jophet_type_to_c_string(&target.jophet_type);
                    let sanitized_name = self.sanitize_c_keyword(&target.var_name);
                    writeln!(
                        &mut self.output,
                        "{}{} {} = {}{}{};",
                        mutability,
                        base_c_type,
                        sanitized_name,
                        temp_var,
                        access_op,
                        format!("f{}", i)
                    )
                    .unwrap();
                }
            }
            JophetType::Struct {
                name: struct_name, ..
            } => {
                if let Some(struct_def) = self.struct_defs_cache.get(struct_name).cloned() {
                    let field_names: Vec<_> =
                        struct_def.fields.iter().map(|(name, _, _)| name.clone()).collect();
                    for (i, target) in decl.targets.iter().enumerate() {
                        if target.var_name == "_" || target.var_name == ".." {
                            continue;
                        }
                        let mutability =
                            if target.is_mutable || self.type_needs_cleanup(&target.jophet_type) {
                                ""
                            } else {
                                "const "
                            };
                        let base_c_type = self.jophet_type_to_c_string(&target.jophet_type);

                        let field_name = target.source_field.as_ref().unwrap_or(&field_names[i]);
                        let sanitized_var_name = self.sanitize_c_keyword(&target.var_name);
                        let sanitized_field_name = self.sanitize_c_keyword(field_name);

                        writeln!(
                            &mut self.output,
                            "{}{} {} = {}{}{};",
                            mutability,
                            base_c_type,
                            sanitized_var_name,
                            temp_var,
                            access_op,
                            sanitized_field_name
                        )
                        .unwrap();
                    }
                } else {
                    writeln!(&mut self.output, "// ERROR: Could not find struct definition for '{}' in C backend.", struct_name).unwrap();
                }
            }
            _ => {
                writeln!(
                    &mut self.output,
                    "// ERROR: Unsupported type for destructuring."
                )
                .unwrap();
            }
        }
    }

    /// Compiles a destructuring declaration statement for an array.
    ///
    /// Example: `[a: Int64, b: Int64] = my_array` becomes:
    /// ```c
    /// int64_t __temp_array_0[2];
    /// memcpy(__temp_array_0, my_array, sizeof(__temp_array_0));
    /// const int64_t a = __temp_array_0[0];
    /// const int64_t b = __temp_array_0[1];
    /// ```
    fn compile_array_destructuring_declaration(
        &mut self,
        decl: &ArrayDestructuringDecl,
        stmt_span: &Span,
    ) {
        let initializer = self.compile_expression(&decl.initializer);
        let temp_var = format!("__temp_array_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        let base_c_type = self.jophet_type_to_c_string(&decl.initializer.jophet_type);
        let dimension_suffix = self.get_array_dimension_suffix(&decl.initializer.jophet_type);

        writeln!(
            &mut self.output,
            "{} {}{};",
            base_c_type, temp_var, dimension_suffix
        )
        .unwrap();
        writeln!(
            &mut self.output,
            "memcpy({}, {}, sizeof({}));",
            temp_var, initializer, temp_var
        )
        .unwrap();

        for (i, target) in decl.targets.iter().enumerate() {
            let mutability =
                if target.is_mutable || self.type_needs_cleanup(&target.jophet_type) {
                    ""
                } else {
                    "const "
                };
            let var_c_type = self.jophet_type_to_c_string(&target.jophet_type);
            let sanitized_name = self.sanitize_c_keyword(&target.var_name);
            let line = self.source_map.line_for_byte(stmt_span.start);
            let size = if let JophetType::Array { size, .. } = &decl.initializer.jophet_type {
                *size
            } else {
                0
            };

            let bounds_check_fn = self.get_bounds_check_helper();
            let initializer_expr = format!(
                "{}[{}({}, {}, \"{}\", {})]",
                temp_var,
                bounds_check_fn,
                i,
                size,
                self.source_map.filename(),
                line
            );

            writeln!(
                &mut self.output,
                "{}{} {} = {};",
                mutability, var_c_type, sanitized_name, initializer_expr
            )
            .unwrap();
        }
    }

    /// Compiles an assignment statement, now handling simple, tuple, and array destructuring assignments.
    fn compile_assignment(&mut self, lvalue: &TypedAssignmentLValue, rvalue: &TypedExpression) {
        match lvalue {
            TypedAssignmentLValue::Expression(left_expr) => {
                let compiled_rvalue = self.compile_expression(rvalue);
                let compiled_lvalue = self.compile_expression(left_expr);
                writeln!(&mut self.output, "{} = {};", compiled_lvalue, compiled_rvalue).unwrap();
            }
            TypedAssignmentLValue::Tuple(targets) => {
                let initializer = self.compile_expression(rvalue);
                let temp_var = format!("__temp_tuple_{}", self.temp_var_counter);
                self.temp_var_counter += 1;
                let tuple_c_type = self.jophet_type_to_c_string(&rvalue.jophet_type);

                // Use a block to scope the temporary tuple variable.
                writeln!(&mut self.output, "{{").unwrap();
                writeln!(
                    &mut self.output,
                    "\t{} {} = {};",
                    tuple_c_type, temp_var, initializer
                )
                .unwrap();

                for (i, target_expr) in targets.iter().enumerate() {
                    let compiled_target = self.compile_expression(target_expr);
                    writeln!(
                        &mut self.output,
                        "\t{} = {}.f{};",
                        compiled_target, temp_var, i
                    )
                    .unwrap();
                }
                writeln!(&mut self.output, "}}").unwrap();
            }
            TypedAssignmentLValue::Array(targets) => {
                let initializer = self.compile_expression(rvalue);

                // We don't need a temporary variable. We can assign directly from the initializer.
                // Still use a block to make the series of assignments an atomic-like operation visually.
                writeln!(&mut self.output, "{{").unwrap();

                for (i, target_expr) in targets.iter().enumerate() {
                    let compiled_target = self.compile_expression(target_expr);
                    let line = self.source_map.line_for_byte(target_expr.span.start);
                    let size = if let JophetType::Array { size, .. } = &rvalue.jophet_type {
                        *size
                    } else {
                        0
                    }; // Should not happen with typed AST

                    let bounds_check_fn = self.get_bounds_check_helper();
                    writeln!(
                        &mut self.output,
                        "\t{} = {}[{}({}, {}, \"{}\", {})];",
                        compiled_target,
                        initializer,
                        bounds_check_fn,
                        i,
                        size,
                        self.source_map.filename(),
                        line
                    )
                    .unwrap();
                }
                writeln!(&mut self.output, "}}").unwrap();
            }
        }
    }
}