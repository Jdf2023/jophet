// src/backend/c/expressions/helpers.rs
//! Contains various helper and utility functions for the C expression compiler.

use super::super::Generator;
use crate::core::ast::typed::*;
use std::fmt::Write;

impl Generator {
    /// A helper function in the C backend to derive the concrete C `Result` struct name
    /// from a `JophetType::Fallible`. This logic was moved here from the semantic analyzer
    /// to decouple the frontend from the C backend's implementation details. It now also
    /// handles generating mangled names for array result types.
    pub fn fallible_type_to_c_result_string(&mut self, fallible_type: &JophetType) -> String {
        if let JophetType::Fallible { ok, err } = fallible_type {
            // The semantic analyzer now provides the concrete error type (e.g., IoError, ParseError),
            // so we can remove the brittle inference logic.
            let ok_c_type = self.jophet_type_to_c_string(ok);

            // Construct the name based on the original Jophet types, not the C types,
            // to correctly match the predefined names in runtime.h.
            let ok_name_part = if **ok == JophetType::Nothing {
                "void".to_string()
            } else if let JophetType::Array { .. } = ok.as_ref() {
                // For arrays, create a mangled name like "Array_int64_t_5"
                format!(
                    "Array_{}_{}",
                    self.jophet_type_to_c_string_for_mangling(&self.get_array_base_type(ok)),
                    self.get_array_total_size(ok)
                )
            } else {
                ok_c_type.clone()
            };
            let err_name_part = if **err == JophetType::Nothing {
                "void".to_string()
            } else {
                self.jophet_type_to_c_string(err)
            };

            let name = format!("Result_{}_{}", ok_name_part, err_name_part)
                .replace('*', "ptr")
                .replace(' ', "");

            // If this is a Result for an Enum from Python, we need to generate its definition.
            if let (JophetType::Enum { name: enum_name, .. }, JophetType::Error { name: err_name, .. }) = (ok.as_ref(), err.as_ref()) {
                if err_name == "FfiError" && !self.predefined_runtime_types.contains(&name) {
                    let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} ok; FfiError err; }} data; }} {};",
                        name, enum_name, name
                    );
                    self.type_defs.insert(struct_def);
                    self.predefined_runtime_types.insert(name.clone());
                }
            }

            // If this is a Result for a Tuple from Python, we need to generate its definition.
            if let (JophetType::Tuple(_), JophetType::Error { name: err_name, .. }) = (ok.as_ref(), err.as_ref()) {
                if err_name == "FfiError" && !self.predefined_runtime_types.contains(&name) {
                     let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} ok; FfiError err; }} data; }} {};",
                        name, ok_c_type, name
                    );
                    self.type_defs.insert(struct_def);
                    self.predefined_runtime_types.insert(name.clone());
                }
            }
            
            // If this is a Result for a Struct from Python, we need to generate its definition.
            if let (JophetType::Struct { name: struct_name, .. }, JophetType::Error { name: err_name, .. }) = (ok.as_ref(), err.as_ref()) {
                if err_name == "FfiError" && !self.predefined_runtime_types.contains(&name) {
                     let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} ok; FfiError err; }} data; }} {};",
                        name, struct_name, name
                    );
                    self.type_defs.insert(struct_def);
                    self.predefined_runtime_types.insert(name.clone());
                }
            }

            // NEW: If this is a Result for an Array from Python, we need to generate its definition.
            if let (JophetType::Array { .. }, JophetType::Error { name: err_name, .. }) = (ok.as_ref(), err.as_ref()) {
                if err_name == "FfiError" && !self.predefined_runtime_types.contains(&name) {
                    // ok_c_type will be just the member type (e.g., "int64_t"). We need the full array type string.
                    let c_array_type = format!("{}{}", self.jophet_type_to_c_string(ok), self.get_array_dimension_suffix(ok));
                     let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} ok; FfiError err; }} data; }} {};",
                        name, c_array_type, name
                    );
                    self.type_defs.insert(struct_def);
                    self.predefined_runtime_types.insert(name.clone());
                }
            }

            // If this is a Result for a Dictionary from Python, we need to generate its definition.
            if let (JophetType::Dictionary { .. }, JophetType::Error { name: err_name, .. }) = (ok.as_ref(), err.as_ref()) {
                 if err_name == "FfiError" && !self.predefined_runtime_types.contains(&name) {
                     let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} ok; FfiError err; }} data; }} {};",
                        name, ok_c_type, name
                    );
                    self.type_defs.insert(struct_def);
                    self.predefined_runtime_types.insert(name.clone());
                }
            }

            // If this is a Result for a TaggedUnion or Error from Python, we need to generate its definition.
            if let (JophetType::TaggedUnion { name: type_name, .. } | JophetType::Error { name: type_name, .. }, JophetType::Error { name: err_name, .. }) = (ok.as_ref(), err.as_ref()) {
                if err_name == "FfiError" && !self.predefined_runtime_types.contains(&name) {
                     let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} ok; FfiError err; }} data; }} {};",
                        name, type_name, name
                    );
                    self.type_defs.insert(struct_def);
                    self.predefined_runtime_types.insert(name.clone());
                }
            }

            // This logic is crucial to match the exact names defined in runtime.h.
            if name == "Result_char_void" {
                return "Result_Char_Nothing".to_string();
            }
            if name == "Result_void_ptr_void" {
                return "Result_void_ptr_void".to_string();
            }
             if name == "Result_void_JophetString" {
                return "Result_void_JophetString".to_string();
            }

            return name;
        }
        // Fallback, though analysis should prevent this from being called on non-fallible types.
        self.jophet_type_to_c_string(fallible_type)
    }

    /// Generates a C statement string that prints a value of a given type.
    ///
    /// This is a recursive helper that generates the appropriate C `printf` calls
    /// or calls to other generated helper functions (like `StructName_print` or `UnionName_print`).
    /// For nested vectors, it now generates and calls a dedicated, type-safe C helper function
    /// to avoid stack corruption issues with deeply nested, generated `for` loops.
    /// It now correctly handles printing of `PythonModule`, `PythonObject`, `CLibrary`, and `Closure` types.
    ///
    /// # Arguments
    /// * `jophet_type` - The Jophet type of the value to be printed.
    /// * `c_expr` - The C expression string that accesses the value (e.g., "my_var", "s->field").
    /// * `is_pointer` - True if `c_expr` is a pointer to the value.
    pub fn get_print_call(&mut self, jophet_type: &JophetType, c_expr: &str, is_pointer: bool) -> String {
        match jophet_type {
            JophetType::Struct { name, .. }
            | JophetType::Union { name, .. }
            | JophetType::TaggedUnion { name, .. }
            | JophetType::Error { name, .. } => {
                let arg = if is_pointer { c_expr.to_string() } else { format!("&{}", c_expr) };
                format!("{}_print({});", name, arg)
            }
            JophetType::Dictionary { key, value } => {
                let arg = if is_pointer { c_expr.to_string() } else { format!("&{}", c_expr) };
                // Register this dictionary type as needing a print function.
                self.dictionaries_to_print.insert((key.as_ref().clone(), value.as_ref().clone()));
                // Generate the call to the unique, generated print function for this dictionary type.
                let key_c_type = self.jophet_type_to_c_string_for_mangling(key);
                let val_c_type = self.jophet_type_to_c_string_for_mangling(value);
                let print_fn_name = format!("__jophet_print_dictionary_of_{}_{}", key_c_type, val_c_type);
                format!("{}({});", print_fn_name, arg)
            }
            JophetType::String => {
                self.runtime_needed = true;
                let (op, expr) = if is_pointer { ("->", c_expr) } else { (".", c_expr) };
                format!("printf(\"%.*s\", (int){expr}{op}len, {expr}{op}data);", expr = expr, op = op)
            }
            JophetType::Reference(inner) | JophetType::MutableReference(inner) | JophetType::Pointer(inner) => {
                match inner.as_ref() {
                    JophetType::Struct { name, .. }
                    | JophetType::Union { name, .. }
                    | JophetType::TaggedUnion { name, .. }
                    | JophetType::Error { name, .. } => format!("{}_print({});", name, c_expr),
                    JophetType::String => {
                        self.runtime_needed = true;
                        format!("printf(\"%.*s\", (int){}->len, {}->data);", c_expr, c_expr)
                    },
                    _ => format!("printf(\"%p\", (void*){});", c_expr), // Print other pointers as addresses
                }
            }
            JophetType::Bool => {
                let access_expr = if is_pointer { format!("*({})", c_expr) } else { c_expr.to_string() };
                format!("printf(\"%s\", ({}) ? \"true\" : \"false\");", access_expr)
            }
            JophetType::Nothing => "printf(\"nothing\");".to_string(),
            JophetType::AnyError => {
                let arg = if is_pointer { c_expr.to_string() } else { format!("&{}", c_expr) };
                format!("JophetError_print({});", arg)
            }
            JophetType::PythonModule | JophetType::PythonObject { .. } => {
                self.python_runtime_needed = true;
                // The `c_expr` is already the PyObject*, which is what the runtime function expects.
                format!("jophet_py_print_object({});", c_expr)
            }
            JophetType::CLibrary { header } => {
                // The c_expr is ignored because the runtime value is just a null pointer.
                // We bake the header name directly into the C printf call.
                format!("printf(\"<C Library: \\\"{}\\\">\");", header.display())
            }
            JophetType::Closure { .. } => {
                let arg = if is_pointer { c_expr.to_string() } else { format!("&{}", c_expr) };
                format!("printf(\"<Closure at %p>\", (void*){});", arg)
            }
            JophetType::Array { member_type, size } => {
                let mut output = String::new();
                let line = self.source_map.line_for_byte(c_expr.len());
                let loop_var = format!("i_{}", self.temp_var_counter);
                self.temp_var_counter += 1; // Ensure unique loop variable for nested calls

                writeln!(&mut output, "printf(\"[\");").unwrap();
                if *size > 0 {
                    writeln!(&mut output, "for (size_t {var} = 0; {var} < {size}; ++{var}) {{", var = loop_var, size = size).unwrap();
                    let element_expr = format!("{array}[{bounds_check}({var}, {size}, \"{file}\", {line})]",
                        array = c_expr,
                        bounds_check = self.get_bounds_check_helper(),
                        var = loop_var,
                        size = size,
                        file = self.source_map.filename(),
                        line = line
                    );
                    let element_print_call = self.get_print_call(member_type, &element_expr, false);
                    writeln!(&mut output, "\t{}", element_print_call).unwrap();
                    writeln!(&mut output, "\tif ({var} < {max}) {{ printf(\", \"); }}", var = loop_var, max = size - 1).unwrap();
                    writeln!(&mut output, "}}").unwrap();
                }
                writeln!(&mut output, "printf(\"]\");").unwrap();
                format!("{{\n{}\n}}", output.trim_end())
            }
            JophetType::Vector(member_type) => {
                self.runtime_needed = true;
                let arg = if is_pointer { c_expr.to_string() } else { format!("&{}", c_expr) };

                let mangled_member_type = self.jophet_type_to_c_string_for_mangling(member_type);
                let print_fn_name = format!("__jophet_print_vector_of_{}", mangled_member_type);

                // Generate the helper function if it doesn't exist yet
                if !self.vector_print_helpers.contains_key(&print_fn_name) {
                    let proto = format!("static void {}(const JophetVector* v);", print_fn_name);
                    self.function_prototypes.insert(proto);
                    
                    let c_member_type = self.jophet_type_to_c_string(member_type);
                    let line = self.source_map.line_for_byte(c_expr.len());
                    
                    let mut def = String::new();
                    let loop_var = format!("i_{}", self.temp_var_counter);
                    self.temp_var_counter += 1;

                    writeln!(&mut def, "static void {}(const JophetVector* v) {{", print_fn_name).unwrap();
                    writeln!(&mut def, "\tprintf(\"[\");").unwrap();
                    writeln!(&mut def, "\tfor (size_t {var} = 0; {var} < v->len; ++{var}) {{", var = loop_var).unwrap();
                    
                    // --- START OF FIX ---
                    // This now correctly gets a POINTER to the element in the vector's buffer.
                    let element_expr = format!("&(({c_type}*)v->data)[{bounds_check}({var}, v->len, \"{file}\", {line})]",
                        c_type = c_member_type,
                        bounds_check = self.get_bounds_check_helper(),
                        var = loop_var,
                        file = self.source_map.filename(),
                        line = line
                    );

                    // Check if the member itself is a primitive or an aggregate struct/tuple.
                    // Primitives are passed by value after dereferencing, aggregates are passed by pointer.
                    let is_member_primitive = self.is_primitive_for_clone(member_type);

                    // If the member is a primitive, we dereference the pointer to get its value.
                    // Otherwise, we pass the pointer directly.
                    let final_element_expr = if is_member_primitive {
                        format!("*({})", element_expr)
                    } else {
                        element_expr
                    };
                    
                    // We now pass `is_pointer = !is_member_primitive`. This is true for structs, tuples,
                    // vectors, etc., ensuring `get_print_call` doesn't take their address again.
                    let element_print_call = self.get_print_call(member_type, &final_element_expr, !is_member_primitive);
                    // --- END OF FIX ---

                    writeln!(&mut def, "\t\t{};", element_print_call).unwrap();
                    writeln!(&mut def, "\t\tif ({var} < v->len - 1) {{ printf(\", \"); }}", var = loop_var).unwrap();
                    writeln!(&mut def, "\t}}").unwrap();
                    writeln!(&mut def, "\tprintf(\"]\");").unwrap();
                    writeln!(&mut def, "}}").unwrap();
                    
                    self.vector_print_helpers.insert(print_fn_name.clone(), def);
                }
                
                format!("{}({});", print_fn_name, arg)
            }
            JophetType::Tuple(types) => {
                let mut output = String::new();
                let (op, expr) = if is_pointer { ("->", c_expr) } else { (".", c_expr) };
                
                writeln!(&mut output, "printf(\"(\");").unwrap();
                for (i, ty) in types.iter().enumerate() {
                    let field_expr = format!("{expr}{op}f{i}", expr = expr, op = op, i = i);
                    let field_print_call = self.get_print_call(ty, &field_expr, false);
                    writeln!(&mut output, "{}", field_print_call).unwrap();
                     if i < types.len() - 1 {
                        writeln!(&mut output, "printf(\", \");").unwrap();
                    }
                }
                writeln!(&mut output, "printf(\")\");").unwrap();
                format!("{{\n{}\n}}", output.trim_end())
            }
            _ => { // Handle all other primitive types
                let format_specifier = self.get_format_specifier(jophet_type);
                let access_expr = if is_pointer { format!("*({})", c_expr) } else { c_expr.to_string() };
                let c_expr_casted = if matches!(jophet_type, JophetType::UInt(64)) {
                    format!("(size_t)({})", access_expr)
                } else {
                    access_expr
                };
                format!("printf({}, {});", format_specifier, c_expr_casted)
            }
        }
    }

    /// Gets the name of the C runtime helper function for bounds checking and ensures
    /// the runtime is marked as needed.
    pub fn get_bounds_check_helper(&mut self) -> &'static str {
        self.runtime_needed = true;
        "jophet_bounds_check"
    }

    /// Ensures an expression is a C l-value, creating a temporary variable if it's an r-value.
    ///
    /// This is a crucial helper for generating C code that needs a stable address for an
    /// expression's result, such as passing arguments by pointer or taking an address (`&`).
    /// If the expression is already an l-value (e.g., a variable, a field access), its
    /// compiled form is returned directly. If it's an r-value (e.g., a literal, the result
    /// of a function call), this function generates C code to store it in a temporary
    /// variable, schedules that variable for cleanup if necessary, and returns the name of
    /// the temporary variable.
    ///
    /// # Returns
    /// A `String` containing the C expression that can be used as an l-value (typically a variable name).
    pub(in crate::backend::c) fn ensure_lvalue(&mut self, expr: &TypedExpression) -> String {
        if self.is_rvalue(&expr.kind) {
            let compiled_expr = self.compile_expression(expr);
            let temp_var = format!("__temp_lvalue_{}", self.temp_var_counter);
            self.temp_var_counter += 1;
            
            let c_type = self.jophet_type_to_c_string(&expr.jophet_type);
            let dimension_suffix = self.get_array_dimension_suffix(&expr.jophet_type);

            writeln!(
                &mut self.output,
                "\t{} {}{} = {};",
                c_type, temp_var, dimension_suffix, compiled_expr
            ).unwrap();
            
            // Schedule for cleanup
            let cleanup = self.get_cleanup_call(&expr.jophet_type, &temp_var, false);
            if !cleanup.is_empty() {
                self.scope_cleanup_stack.last_mut().unwrap().push(cleanup);
            }
            temp_var
        } else {
            // It's already an l-value (variable, field access, etc.), so we can just compile it.
            self.compile_expression(expr)
        }
    }
    
    /// Helper to identify expressions that are temporary r-values and not l-values.
    ///
    /// An expression is an r-value if it's a temporary result, such as a literal, a function call,
    /// or an arithmetic operation. L-values are expressions that refer to a memory location, like
    /// variables, field accesses, or array indexes. This method delegates to the `is_lvalue`
    /// method on `TypedExpressionKind`.
    pub fn is_rvalue(&self, kind: &TypedExpressionKind) -> bool {
        !kind.is_lvalue()
    }

    /// Gets or creates a C helper function to "sprint" (string-print) a complex type.
    /// This is used for interpolated strings. It caches the generated functions to avoid duplicates.
    pub fn get_or_create_sprint_fn(&mut self, ty: &JophetType) -> (String, Option<String>) {
        let c_type_name = self.jophet_type_to_c_string(ty);
        // Sanitize the type name for use in a C function name.
        let safe_type_name = c_type_name.replace('*', "ptr").replace(' ', "_");
        let fn_name = format!("__jophet_sprint_{}", safe_type_name);

        // If we've already generated this helper, just return its name.
        if self.sprint_helpers.contains(&fn_name) {
            return (fn_name, None);
        }

        let mut body = String::new();
        let mut def = String::new();

        writeln!(&mut def, "void {}(JophetString* builder, const void* data) {{", fn_name).unwrap();
        writeln!(&mut def, "\tconst {}* s = (const {}*)data;", c_type_name, c_type_name).unwrap();

        // Generate the body of the sprint function based on the type.
        // This is essentially a reimplementation of the `_print` logic, but appending to a builder.
        match ty {
            JophetType::Vector(member_type) => {
                let c_member_type = self.jophet_type_to_c_string(member_type);
                let loop_var = format!("i_{}", self.temp_var_counter);
                self.temp_var_counter += 1;

                writeln!(&mut body, "\tString_builder_append(builder, \"[\");").unwrap();
                writeln!(&mut body, "\tfor (size_t {var} = 0; {var} < s->len; ++{var}) {{", var = loop_var).unwrap();
                
                let element_expr = format!("&((({}*)s->data)[{}])", c_member_type, loop_var);
                
                // We need to generate a call to the correct append function for the member type.
                match **member_type {
                    JophetType::Char => {
                         writeln!(&mut body, "\t\tString_builder_append_char(builder, *(char*){});", element_expr).unwrap();
                    },
                    JophetType::Int(_) => {
                         writeln!(&mut body, "\t\tString_builder_append_int64(builder, *(int64_t*){});", element_expr).unwrap();
                    },
                    // Add other primitive types here...
                    _ => {
                        // For nested complex types, we would recurse.
                        let (sprint_fn, sprint_def) = self.get_or_create_sprint_fn(member_type);
                        if let Some(s_def) = sprint_def {
                            if self.sprint_helpers.insert(sprint_fn.clone()) {
                                 writeln!(&mut self.function_defs, "{}\n", s_def).unwrap();
                            }
                        }
                        writeln!(&mut body, "\t\tjophet_sprint(builder, {}, (void (*)(JophetString*, const void*))&{});", element_expr, sprint_fn).unwrap();
                    }
                }
                
                writeln!(&mut body, "\t\tif ({} < s->len - 1) {{ String_builder_append(builder, \", \"); }}", loop_var).unwrap();
                writeln!(&mut body, "\t}}").unwrap();
                writeln!(&mut body, "\tString_builder_append(builder, \"]\");").unwrap();
            }
            // Add other complex types like Struct, Tuple here if needed.
             _ => {
                // Fallback for types that have a `_print` function by calling it and redirecting stdout.
                // This is complex. For now, let's just use the type name as a placeholder.
                writeln!(&mut body, "\tString_builder_append(builder, \"<{}>\");", c_type_name).unwrap();
            }
        }
        
        writeln!(&mut def, "{}", body).unwrap();
        writeln!(&mut def, "}}").unwrap();

        (fn_name, Some(def))
    }

    /// Helper function to compile the body of a `switch` case or `else` block.
    ///
    /// It iterates through the statements in a branch. If a `yield` statement is found,
    /// it compiles it as an assignment to the `result_var`. Other statements are
    /// compiled normally.
    ///
    /// # Panics
    /// Panics if writing to the internal output buffer fails.
    pub fn compile_switch_branch_body(
        &mut self,
        body: &[TypedStatement],
        result_var: Option<&str>,
    ) {
        for stmt in body {
            if let TypedStatementKind::Yield(yield_expr) = &stmt.kind {
                if let Some(var) = result_var {
                    let compiled_yield = self.compile_expression(yield_expr);
                    writeln!(&mut self.output, "\t\t\t{} = {};", var, compiled_yield)
                        .expect("Failed to write to internal buffer");
                }
            } else {
                // Temporarily redirect output to handle indentation correctly.
                let mut temp_output = String::new();
                std::mem::swap(&mut self.output, &mut temp_output);

                self.compile_statement_common(stmt, false);

                std::mem::swap(&mut self.output, &mut temp_output);

                for line in temp_output.lines() {
                    writeln!(&mut self.output, "\t\t\t{}", line)
                        .expect("Failed to write to internal buffer");
                }
            }
        }
    }
}