// src/backend/c/expressions/control_flow.rs
//! Handles compilation of expressions that involve control flow, such as `switch`, `try`, and `catch`.

use super::super::Generator;
use super::CExpression;
use crate::core::ast::typed::*;
use std::fmt::Write;

impl Generator {
    /// Generates the C expression to access the unwrapped 'ok' value from a fallible result,
    /// handling the special void* case for `Dictionary.get`.
    fn get_unwrapped_ok_expr(&mut self, fallible_expr: &TypedExpression, temp_result_var: &str) -> String {
        if let TypedExpressionKind::MethodCall { object, mangled_name, .. } = &fallible_expr.kind {
            if mangled_name == "get" && matches!(object.jophet_type, JophetType::Dictionary { .. }) {
                let value_type_c_string = if let JophetType::Fallible { ok, .. } = &fallible_expr.jophet_type {
                    self.jophet_type_to_c_string(ok)
                } else {
                    unreachable!("Dictionary.get must return a fallible type.")
                };
                return format!("*({}*)({}.data.ok)", value_type_c_string, temp_result_var);
            }
        }
        format!("{}.data.ok", temp_result_var)
    }
    
    /// Compiles a `try` expression for error propagation.
    ///
    /// This is a complex expression that generates statements. It compiles the inner
    /// fallible expression, stores its result in a temporary variable, and then
    /// generates a C `if` statement to check the `is_ok` flag. If it's a failure,
    /// it generates a C `return` statement to propagate the error up the call stack,
    /// correctly upcasting the specific error into the universal `JophetError` if needed.
    /// If it succeeds, the expression evaluates to the unwrapped `ok` value.
    ///
    /// The final result is returned as a `Simple` expression containing the unwrapped value,
    /// as the temporary variable for the `Result` struct is not the final value of the expression.
    ///
    /// # Panics
    /// Panics if writing to the internal output buffer fails.
    pub(super) fn compile_propagate_error_expression(
        &mut self,
        propagate_expr: &TypedExpression,
    ) -> CExpression {
        let compiled_expr = self.compile_expression(propagate_expr);
        let temp_result_var = format!("__propagate_res_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        let fallible_c_type = self.fallible_type_to_c_result_string(&propagate_expr.jophet_type);
        writeln!(
            &mut self.output,
            "\t{} {} = {};",
            fallible_c_type, temp_result_var, compiled_expr
        )
        .expect("Failed to write to internal buffer");

        // We clone here to break the immutable borrow on `self` before we call the
        // mutable `jophet_type_to_c_string` method.
        let function_return_type = self
            .current_function_return_type
            .as_ref()
            .expect("`try` must be inside a function, but no return type was set in generator")
            .clone();
        let function_return_c_type = self.jophet_type_to_c_string(&function_return_type);

        // Generate the check-and-return block.
        writeln!(&mut self.output, "\tif (!{}.is_ok) {{", temp_result_var).unwrap();

        // Handle error upcasting to AnyError
        if let JophetType::Fallible {
            err: func_err_ty, ..
        } = &function_return_type
        {
            if let JophetType::AnyError = func_err_ty.as_ref() {
                // The function returns AnyError, so we MUST upcast.
                let specific_err_ty = if let JophetType::Fallible { err, .. } =
                    &propagate_expr.jophet_type
                {
                    err
                } else {
                    unreachable!("Propagate expression must have a fallible type.")
                };

                let specific_err_name = if let JophetType::Error { name, .. } =
                    specific_err_ty.as_ref()
                {
                    name
                } else {
                    unreachable!("Propagated error must be a JophetType::Error to be upcast")
                };

                // Generate the C code to construct a universal JophetError.
                writeln!(
                    &mut self.output,
                    "\t\tJophetError upcast_err = (JophetError){{ .tag = JophetError_{}, .data.{} = {}.data.err }};",
                    specific_err_name, specific_err_name, temp_result_var
                )
                .unwrap();
                writeln!(
                    &mut self.output,
                    "\t\treturn ({}){{ .is_ok = false, .data.err = upcast_err }};",
                    function_return_c_type
                )
                .unwrap();
            } else {
                // The error types are the same, direct assignment is fine.
                writeln!(
                    &mut self.output,
                    "\t\treturn ({}){{ .is_ok = false, .data.err = {}.data.err }};",
                    function_return_c_type, temp_result_var
                )
                .unwrap();
            }
        }

        writeln!(&mut self.output, "\t}}").unwrap();

        // If the check passes, the expression's value is the unwrapped 'ok' part.
        let final_value_expr = self.get_unwrapped_ok_expr(propagate_expr, &temp_result_var);
        CExpression::Simple(final_value_expr)
    }

    /// Compiles a `try` expression used in a non-fallible context.
    ///
    /// This generates code that checks the result of a fallible expression. If it's
    /// an error, it calls a C runtime helper to print a formatted panic message (with
    /// source location) and terminate the program. If it succeeds, the expression
    /// evaluates to the unwrapped `ok` value.
    ///
    /// The final result is returned as a `Simple` expression.
    ///
    /// # Panics
    /// Panics if writing to the internal output buffer fails.
    pub(super) fn compile_unwrap_or_panic_expression(
        &mut self,
        unwrap_expr: &TypedExpression,
    ) -> CExpression {
        let compiled_expr = self.compile_expression(unwrap_expr);
        let temp_result_var = format!("__unwrap_res_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        let fallible_c_type = self.fallible_type_to_c_result_string(&unwrap_expr.jophet_type);
        let line = self.source_map.line_for_byte(unwrap_expr.span.start);
        writeln!(
            &mut self.output,
            "\t{} {} = {};",
            fallible_c_type, temp_result_var, compiled_expr
        )
        .expect("Failed to write to internal buffer");

        let err_type = if let JophetType::Fallible { err, .. } = &unwrap_expr.jophet_type {
            err.as_ref()
        } else {
            unreachable!("Unwrap expression must have a fallible type.")
        };
        let err_print_fn = format!("{}_print", self.jophet_type_to_c_string(err_type));

        writeln!(&mut self.output, "\tif (!{}.is_ok) {{", temp_result_var).unwrap();
        writeln!(&mut self.output, "\t\tjophet_panic_on_err(&{}.data.err, (void (*)(const void*))&{}, \"{}\", {});", temp_result_var, err_print_fn, self.source_map.filename(), line).unwrap();
        writeln!(&mut self.output, "\t}}").unwrap();

        let final_value_expr = self.get_unwrapped_ok_expr(unwrap_expr, &temp_result_var);
        CExpression::Simple(final_value_expr)
    }

    /// Compiles a `catch` expression.
    ///
    /// This is a complex expression that generates a block of C code:
    /// 1. The fallible expression is evaluated *once*. If the compilation of the inner expression
    ///    already produced a temporary variable (as indicated by the `CExpression::Temporary` variant),
    ///    that variable is used directly. Otherwise, a new temporary variable is created.
    /// 2. A second temporary variable is declared to hold the final result of the `catch` expression.
    /// 3. An `if` statement checks the `is_ok` flag of the first temporary variable.
    /// 4. If not ok, the `err` value is assigned to the error variable, and the `catch` body is executed.
    ///    The body must contain a `yield` statement, which is compiled into an assignment to the result variable.
    /// 5. If ok, the `ok` value is assigned to the result variable. This now correctly handles
    ///    the special `void*` return from `Dictionary.get`.
    /// 6. The name of the result variable is returned as a `Temporary`.
    ///
    /// # Panics
    /// Panics if writing to the internal output buffer fails.
    pub(super) fn compile_catch_expression(
        &mut self,
        return_type: &JophetType,
        expression: &TypedExpression,
        error_variable: &str,
        body: &[TypedStatement],
    ) -> CExpression {
        // Compile the inner expression. This might be a simple expression or might have created a temporary.
        let compiled_fallible_expr = self.compile_expression_internal(expression);

        // If the inner expression was complex and already created a temporary variable,
        // we can reuse it. Otherwise, we create a new temporary variable for the result.
        let result_var_name = match compiled_fallible_expr {
            CExpression::Temporary(name) => name,
            CExpression::Simple(expr_str) => {
                let fallible_c_type = self.fallible_type_to_c_result_string(&expression.jophet_type);
                let temp_fallible_var = format!("__catch_fallible_{}", self.temp_var_counter);
                self.temp_var_counter += 1;
                writeln!(
                    &mut self.output,
                    "\t{} {} = {};",
                    fallible_c_type, temp_fallible_var, expr_str
                )
                .expect("Failed to write to internal buffer");
                temp_fallible_var
            }
        };

        let c_type = self.jophet_type_to_c_string(return_type);

        let (err_type, _ok_type) = if let JophetType::Fallible { err, ok } = &expression.jophet_type {
            (self.jophet_type_to_c_string(err), self.jophet_type_to_c_string(ok))
        } else {
            unreachable!("`catch` expression must operate on a fallible type.")
        };

        // If the entire catch expression evaluates to `nothing`, we don't need a result variable.
        if *return_type == JophetType::Nothing {
            writeln!(&mut self.output, "\tif (!{}.is_ok) {{", result_var_name).unwrap();

            // Declare the error variable inside the `if` block.
            let sanitized_error_variable = self.sanitize_c_keyword(error_variable);
            writeln!(
                &mut self.output,
                "\t\t{} {} = {}.data.err;",
                err_type, sanitized_error_variable, result_var_name
            )
            .expect("Failed to write to internal buffer");

            let mut body_output = String::new();
            std::mem::swap(&mut self.output, &mut body_output);
            self.compile_switch_branch_body(body, None);
            std::mem::swap(&mut self.output, &mut body_output);
            for line in body_output.lines() {
                writeln!(&mut self.output, "\t\t{}", line)
                    .expect("Failed to write to internal buffer");
            }
            writeln!(&mut self.output, "\t}}").unwrap();
            return CExpression::Simple("(void)0".to_string());
        }

        let temp_var = format!("__catch_res_{}", self.temp_var_counter);
        self.temp_var_counter += 1;
        writeln!(&mut self.output, "\t{} {};", c_type, temp_var)
            .expect("Failed to write to internal buffer");

        writeln!(&mut self.output, "\tif (!{}.is_ok) {{", result_var_name)
            .expect("Failed to write to internal buffer");

        // For Dictionary.get, err is a dummy, so no variable is created.
        if err_type != "void" && err_type != "int" {
            let sanitized_error_variable = self.sanitize_c_keyword(error_variable);
            writeln!(
                &mut self.output,
                "\t\t{} {} = {}.data.err;",
                err_type, sanitized_error_variable, result_var_name
            )
            .expect("Failed to write to internal buffer");
        }

        // Temporarily swap buffers to compile the body of the catch block, handling yield correctly.
        let mut body_output = String::new();
        std::mem::swap(&mut self.output, &mut body_output);

        self.compile_switch_branch_body(body, Some(&temp_var));

        std::mem::swap(&mut self.output, &mut body_output);
        // Indent the compiled body
        for line in body_output.lines() {
            writeln!(&mut self.output, "\t\t{}", line)
                .expect("Failed to write to internal buffer");
        }

        writeln!(&mut self.output, "\t}} else {{").expect("Failed to write to internal buffer");

        // The 'ok' value is assigned using the helper to handle special cases.
        let unwrapped_ok_expr = self.get_unwrapped_ok_expr(expression, &result_var_name);
        writeln!(&mut self.output, "\t\t{} = {};", temp_var, unwrapped_ok_expr)
            .expect("Failed to write to internal buffer");
        
        writeln!(&mut self.output, "\t}}").expect("Failed to write to internal buffer");

        CExpression::Temporary(temp_var)
    }

    /// Compiles a `switch` expression.
    ///
    /// This can generate two different C constructs:
    /// 1. A standard C `switch` statement, if the type being matched on is an integer-like type,
    ///    a tagged union, or an error type. It now handles destructuring patterns for tagged types.
    /// 2. A chain of `if-else if` statements for other types (e.g., strings).
    ///
    /// It now correctly generates C enum tags as `case` labels for tagged unions,
    /// fixing a bug where it previously generated invalid compound literals.
    ///
    /// If the `switch` is an expression (i.e., it returns a value via `yield`), a temporary
    /// variable is created to store the result from the executed branch. This function returns a `Temporary`
    /// if it's an expression, otherwise `Simple`.
    ///
    /// # Panics
    /// Panics if writing to the internal output buffer fails.
    pub(super) fn compile_switch_expression(
        &mut self,
        return_type: &JophetType,
        expression: &TypedExpression,
        cases: &[TypedSwitchCase],
        else_block: &Option<Vec<TypedStatement>>,
    ) -> CExpression {
        let is_expression = *return_type != JophetType::Nothing;
        let compiled_expr = self.compile_expression(expression);

        let result_var = if is_expression {
            let name = format!("__switch_res_{}", self.temp_var_counter);
            self.temp_var_counter += 1;
            let c_type = self.jophet_type_to_c_string(return_type);
            writeln!(&mut self.output, "\t{} {};", c_type, name)
                .expect("Failed to write to internal buffer");
            Some(name)
        } else {
            None
        };

        let use_native_switch = matches!(
            expression.jophet_type,
            JophetType::Int(_)
                | JophetType::UInt(_)
                | JophetType::Enum { .. }
                | JophetType::Char
                | JophetType::Bool
                | JophetType::TaggedUnion { .. }
                | JophetType::Error { .. }
        );

        if !use_native_switch {
            // Use if-else chain for non-switchable types like strings.
            let mut first_case = true;
            for case in cases {
                let condition = case
                    .patterns
                    .iter()
                    .map(|p| match p {
                        TypedPattern::Literal(expr) => {
                            format!("({}) == ({})", compiled_expr, self.compile_expression(expr))
                        }
                        _ => unreachable!("Destructuring patterns not allowed in if-else switch"),
                    })
                    .collect::<Vec<_>>()
                    .join(" || ");

                if first_case {
                    writeln!(&mut self.output, "\tif ({}) {{", condition)
                        .expect("Failed to write to internal buffer");
                    first_case = false;
                } else {
                    writeln!(&mut self.output, "\t}} else if ({}) {{", condition)
                        .expect("Failed to write to internal buffer");
                }
                self.compile_switch_branch_body(&case.body, result_var.as_deref());
            }
            if let Some(else_stmts) = else_block {
                writeln!(&mut self.output, "\t}} else {{")
                    .expect("Failed to write to internal buffer");
                self.compile_switch_branch_body(else_stmts, result_var.as_deref());
            }
            writeln!(&mut self.output, "\t}}").expect("Failed to write to internal buffer");
        } else {
            // Use a native C switch statement.
            let switch_on_expr =
                if matches!(expression.jophet_type, JophetType::TaggedUnion {..} | JophetType::Error {..}) {
                    format!("{}.tag", compiled_expr)
                } else {
                    compiled_expr.clone()
                };
            writeln!(&mut self.output, "\tswitch ({}) {{", switch_on_expr)
                .expect("Failed to write to internal buffer");

            for case in cases {
                for pattern in &case.patterns {
                    let pattern_str = match pattern {
                        TypedPattern::Literal(expr) => {
                            if let TypedExpressionKind::TaggedUnionInstantiation { enum_name, variant_name, .. } = &expr.kind {
                                // ALWAYS use the C enum tag for the case label.
                                format!("{}_{}", enum_name, variant_name)
                            } else {
                                // This handles integers, C-style enums, etc.
                                self.compile_expression(expr)
                            }
                        }
                        TypedPattern::Destructure {
                            enum_type,
                            variant_name,
                            ..
                        } => {
                            let enum_c_name = self.jophet_type_to_c_string(enum_type);
                            format!("{}_{}", enum_c_name, variant_name)
                        }
                    };
                    writeln!(&mut self.output, "\t\tcase {}:", pattern_str)
                        .expect("Failed to write to internal buffer");
                }
                writeln!(&mut self.output, "\t\t{{").expect("Failed to write to internal buffer");

                if let Some(TypedPattern::Destructure {
                    binding: Some((var_name, var_type)),
                    variant_name,
                    ..
                }) = case.patterns.first()
                {
                    let var_c_type = self.jophet_type_to_c_string(var_type);
                    let sanitized_var_name = self.sanitize_c_keyword(var_name);
                    let initializer = format!("{}.data.{}", compiled_expr, variant_name);
                    writeln!(
                        &mut self.output,
                        "\t\t\t{} {} = {};",
                        var_c_type, sanitized_var_name, initializer
                    )
                    .unwrap();
                }

                self.compile_switch_branch_body(&case.body, result_var.as_deref());
                writeln!(&mut self.output, "\t\t\tbreak;")
                    .expect("Failed to write to internal buffer");
                writeln!(&mut self.output, "\t\t}}").expect("Failed to write to internal buffer");
            }
            if let Some(else_stmts) = else_block {
                writeln!(&mut self.output, "\t\tdefault:")
                    .expect("Failed to write to internal buffer");
                writeln!(&mut self.output, "\t\t{{").expect("Failed to write to internal buffer");
                self.compile_switch_branch_body(else_stmts, result_var.as_deref());
                writeln!(&mut self.output, "\t\t\tbreak;")
                    .expect("Failed to write to internal buffer");
                writeln!(&mut self.output, "\t\t}}").expect("Failed to write to internal buffer");
            }
            writeln!(&mut self.output, "\t}}").expect("Failed to write to internal buffer");
        }

        if let Some(name) = result_var {
            CExpression::Temporary(name)
        } else {
            CExpression::Simple("".to_string())
        }
    }
}