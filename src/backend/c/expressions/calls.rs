// src/backend/c/expressions/calls.rs
//! Handles compilation of function calls, method calls, and related built-ins.

use super::super::Generator;
use super::CExpression;
use crate::core::ast::typed::*;
use std::fmt::Write;
use std::path::PathBuf;

impl Generator {
    /// Compiles a standard function call or a closure call expression.
    ///
    /// This function handles calls to user-defined Jophet functions, closures, C runtime functions,
    /// and built-in math functions. For C runtime and file I/O functions, it creates
    /// temporary variables for r-value struct arguments to ensure a stable address can be taken.
    /// For closures, it correctly extracts the `env_struct_name` from the closure's
    /// `JophetType` and includes the environment pointer in the function signature for the C cast.
    /// It also handles the special `slice()` built-in for Python FFI and generates portable C
    /// helper functions for Python `min`/`max` on vectors.
    ///
    /// This function returns a `CExpression::Simple` as function calls in C are always simple expressions,
    /// even if they require pre-statements (like creating temporary arguments) to be emitted.
    pub fn compile_function_call_expression(
        &mut self,
        kind: &TypedCallKind,
        args: &[TypedExpression],
        call_expr: &TypedExpression,
    ) -> CExpression {
        // --- Handle Closure Call ---
        if let TypedCallKind::Closure {
            callable_expr,
            params,
            ret,
        } = kind
        {
            let compiled_callable = self.compile_expression(callable_expr);

            // The information needed is now directly in the Closure type.
            let (_mangled_name, env_struct_name) =
                if let JophetType::Closure {
                    mangled_name,
                    env_struct_name,
                    ..
                } = &callable_expr.jophet_type
                {
                    (mangled_name, env_struct_name)
                } else {
                    unreachable!("Type mismatch: expected a Closure type for a closure call.")
                };

            let c_ret_type = self.jophet_type_to_c_string(ret);

            let mut c_param_types = Vec::new();
            // The C function for a closure *always* has an env parameter.
            c_param_types.push(format!("{}*", env_struct_name));
            for p_type in params {
                c_param_types.push(self.jophet_type_to_c_string(p_type));
            }

            // Construct the full C function pointer type for casting.
            let fn_ptr_type = format!("{} (*)( {} )", c_ret_type, c_param_types.join(", "));

            let mut compiled_args: Vec<String> =
                args.iter().map(|a| self.compile_expression(a)).collect();
            // Prepend the environment pointer to the argument list for the call.
            compiled_args.insert(0, format!("{}.env", compiled_callable));

            // The final C expression casts the void function pointer and calls it.
            let result = format!(
                "(({})({}.fn_ptr))({})",
                fn_ptr_type,
                compiled_callable,
                compiled_args.join(", ")
            );
            return CExpression::Simple(result);
        }

        let mangled_name = if let TypedCallKind::Named(name) = kind {
            name
        } else {
            unreachable!("Expected a named call kind for a non-closure function call.")
        };

        let compiled_args: Vec<String> =
            args.iter().map(|a| self.compile_expression(a)).collect();

        // Handle special built-ins that are not simple C function calls
        let result = match mangled_name.as_str() {
            "jophet_allocate" => {
                self.runtime_needed = true;
                format!("malloc({})", compiled_args[0])
            }
            "jophet_deallocate" => {
                self.runtime_needed = true;
                format!("free({})", compiled_args[0])
            }
            "__jophet_collection_minimum_or_panic" | "__jophet_collection_maximum_or_panic" => {
                self.runtime_needed = true;
                let collection_arg = &args[0];
                let (member_type, size_expr, data_ptr_expr) = match &collection_arg.jophet_type {
                    JophetType::Array { member_type, size } => (
                        member_type.as_ref(),
                        size.to_string(),
                        format!("(void*){}", compiled_args[0]),
                    ),
                    JophetType::Vector(member_type) => (
                        member_type.as_ref(),
                        format!("{}.len", compiled_args[0]),
                        format!("{}.data", compiled_args[0]),
                    ),
                    _ => unreachable!("Semantic analyzer should prevent non-collections here."),
                };
                let c_member_type = self.jophet_type_to_c_string(member_type);
                let comparison_fn_name = self.get_or_create_comparison_thunk(member_type, mangled_name.contains("maximum"));
                let line = self.source_map.line_for_byte(call_expr.span.start);
                let file = self.source_map.filename();
                // The runtime function returns a void*, which we must cast and dereference.
                format!(
                    "*({}*){}({}, {}, sizeof({}), &{}, \"{}\", {})",
                    c_member_type,
                    mangled_name,
                    data_ptr_expr,
                    size_expr,
                    c_member_type,
                    comparison_fn_name,
                    file,
                    line
                )
            }
            "__jophet_string_minimum_or_panic" | "__jophet_string_maximum_or_panic" => {
                self.runtime_needed = true;
                let collection_arg = &args[0];
                let (len_expr, data_ptr_expr) = match &collection_arg.jophet_type {
                    JophetType::String => (
                        format!("{}.len", compiled_args[0]),
                        format!("{}.data", compiled_args[0]),
                    ),
                    JophetType::StringSlice => (
                        format!("strlen({})", compiled_args[0]),
                        compiled_args[0].clone(),
                    ),
                    _ => unreachable!("Semantic analysis ensures this is only called on string-like types."),
                };
                let line = self.source_map.line_for_byte(call_expr.span.start);
                let file = self.source_map.filename();
                format!("{}({}, {}, \"{}\", {})", mangled_name, data_ptr_expr, len_expr, file, line)
            }
            "__jophet_python_minimum_or_panic" | "__jophet_python_maximum_or_panic" => {
                self.python_runtime_needed = true;
                let python_builtin_name = if mangled_name.contains("maximum") { "max" } else { "min" };
                
                // Perform immutable borrows first and store results.
                let line = self.source_map.line_for_byte(call_expr.span.start);
                let file = self.source_map.filename().to_string(); // Clone to release borrow.

                // Check if the argument is a native PythonObject or a Jophet Vector of PythonObjects
                match &args[0].jophet_type {
                    JophetType::PythonObject { .. } => {
                        format!(
                            "jophet_py_call_builtin_or_panic(\"{}\", {}, \"{}\", {})",
                            python_builtin_name, compiled_args.join(", "), file, line
                        )
                    },
                    JophetType::Vector(member_type) if matches!(member_type.as_ref(), JophetType::PythonObject { .. }) => {
                        // **PORTABILITY FIX**: Generate a portable C helper function.
                        let helper_name = self.get_or_create_py_minmax_vector_helper(python_builtin_name);
                        format!("{}(&{}, \"{}\", {})", helper_name, compiled_args[0], file, line)
                    },
                    _ => unreachable!("Semantic analysis should prevent non-Python-comparable types here."),
                }
            }
            "input" => {
                self.runtime_needed = true;
                if let Some(prompt) = args.get(0) {
                    let compiled_prompt = self.compile_expression(prompt);
                    let c_str_arg = match prompt.jophet_type {
                        JophetType::String => format!("{}.data", compiled_prompt),
                        _ => compiled_prompt,
                    };
                    format!("input({})", c_str_arg)
                } else {
                    "input(NULL)".to_string()
                }
            }
            "slice" => {
                // This is a special built-in for Python FFI slicing.
                // It translates to a direct call to PySlice_New.
                self.python_runtime_needed = true;
                let start = if args[0].jophet_type == JophetType::Nothing {
                    "NULL".to_string()
                } else {
                    format!("PyLong_FromLong({})", self.compile_expression(&args[0]))
                };
                let end = if args[1].jophet_type == JophetType::Nothing {
                    "NULL".to_string()
                } else {
                    format!("PyLong_FromLong({})", self.compile_expression(&args[1]))
                };
                // The `step` argument is always NULL for Jophet's `[start:end]` syntax.
                format!("PySlice_New({}, {}, NULL)", start, end)
            }
            "deg2rad" => {
                format!("({} * (M_PI / 180.0))", compiled_args[0])
            }
            "rad2deg" => {
                format!("({} * (180.0 / M_PI))", compiled_args[0])
            }
            "jophet_collect" => {
                self.runtime_needed = true;
                let element_type = if let JophetType::Vector(el) = &call_expr.jophet_type {
                    el.as_ref()
                } else {
                    unreachable!("`collect` must be known to return a Vector.")
                };
                let c_element_type = self.jophet_type_to_c_string(element_type);
                let mut final_args =
                    vec![format!("sizeof({})", c_element_type), format!("{}", args.len())];
                final_args.extend(compiled_args);
                format!("{}({})", mangled_name, final_args.join(", "))
            }
            "__jophet_variadic_command" => {
                self.runtime_needed = true;
                // Prepend the argument count to the list of arguments.
                let mut final_args = vec![format!("{}", args.len())];
                final_args.extend(compiled_args);
                // Call the actual C runtime function.
                format!("jophet_command({})", final_args.join(", "))
            }
            _ => {
                // C runtime functions (e.g., file I/O, parsing) expect pointers for struct-like types.
                let is_runtime_pointer_call =
                    mangled_name.starts_with("jophet_") || mangled_name.starts_with("parse_");

                let final_args: Vec<String> = args
                    .iter()
                    .zip(compiled_args.iter())
                    .map(|(arg, compiled_arg)| {
                        if is_runtime_pointer_call && self.is_struct_like(&arg.jophet_type) {
                            // The `ensure_lvalue` helper handles the temporary variable pattern.
                            let lvalue = self.ensure_lvalue(arg);
                            format!("&{}", lvalue)
                        } else {
                            compiled_arg.clone()
                        }
                    })
                    .collect();

                format!("{}({})", mangled_name, final_args.join(", "))
            }
        };
        CExpression::Simple(result)
    }


    /// Compiles a method call expression (e.g., `vec.push(item)`).
    ///
    /// This is the main dispatcher for method calls. It identifies the type of call
    /// (C FFI, Python FFI, built-in, or user-defined) and delegates to a specialized
    /// helper function to generate the appropriate C code.
    /// It correctly handles the receiver argument: primitives are passed by value, while
    /// structs are passed by pointer. It uses `ensure_lvalue` to create temporary variables
    /// for r-value struct receivers to ensure a stable address can be taken.
    pub fn compile_method_call_expression(
        &mut self,
        object: &TypedExpression,
        mangled_name: &str,
        args: &[TypedExpression],
        return_type: &JophetType,
    ) -> CExpression {
        let compiled_obj_expr = self.compile_expression(object);

        // 1. Dispatch C FFI calls
        if let JophetType::CLibrary { .. } = &object.jophet_type {
            return self.compile_c_ffi_method_call(mangled_name, args);
        }

        // 2. Dispatch Python FFI calls
        if let JophetType::PythonModule | JophetType::PythonObject { .. } = &object.jophet_type {
            return self.compile_python_ffi_method_call(
                object,
                compiled_obj_expr,
                mangled_name,
                args,
            );
        }

        // 3. Dispatch built-in methods
        if let Some(result) = self.try_compile_builtin_method_call(
            object,
            &compiled_obj_expr,
            mangled_name,
            args,
            return_type,
        ) {
            return result;
        }

        // 4. Default to user-defined method call
        let compiled_args: Vec<String> =
            args.iter().map(|a| self.compile_expression(a)).collect();
        let is_pointer_receiver = matches!(
            object.jophet_type,
            JophetType::Pointer(_) | JophetType::Reference(_) | JophetType::MutableReference(_)
        );

        let receiver_arg = if is_pointer_receiver {
            compiled_obj_expr
        } else if self.is_primitive_for_clone(&object.jophet_type) {
            // Primitives are always passed by value.
            compiled_obj_expr
        } else {
            // Structs are passed by pointer for method calls.
            // Use `ensure_lvalue` to handle the temporary variable pattern cleanly.
            let receiver_lvalue = self.ensure_lvalue(object);
            format!("&{}", receiver_lvalue)
        };

        let mut final_args = vec![receiver_arg];
        final_args.extend(compiled_args);
        let result = format!("{}({})", mangled_name, final_args.join(", "));

        CExpression::Simple(result)
    }

    /// Compiles a method call on a `CLibrary` handle.
    /// This translates directly to a C function call where the method name is the C function name.
    fn compile_c_ffi_method_call(
        &mut self,
        mangled_name: &str,
        args: &[TypedExpression],
    ) -> CExpression {
        let compiled_args: Vec<String> =
            args.iter().map(|a| self.compile_expression(a)).collect();
        CExpression::Simple(format!("{}({})", mangled_name, compiled_args.join(", ")))
    }

    /// Compiles a method call on a `PythonModule` or `PythonObject`.
    /// This generates calls to the Jophet Python C-API runtime helpers. It has been refactored
    /// to use private helpers for each specific magic method call.
    fn compile_python_ffi_method_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: String,
        mangled_name: &str,
        args: &[TypedExpression],
    ) -> CExpression {
        match mangled_name {
            "flatten" => self.compile_py_flatten_call(object, compiled_obj_expr),
            "length" => self.compile_py_length_call(object, compiled_obj_expr),
            "__getitem__" => self.compile_py_getitem_call(object, compiled_obj_expr, args),
            "__getattr__" => self.compile_py_getattr_call(object, compiled_obj_expr, args),
            _ => self.compile_py_generic_method_call(object, compiled_obj_expr, mangled_name, args),
        }
    }

    /// Helper for `compile_python_ffi_method_call`: handles `.flatten()`.
    fn compile_py_flatten_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: String,
    ) -> CExpression {
        self.python_runtime_needed = true;
        let line = self.source_map.line_for_byte(object.span.start);
        let filename = self.source_map.filename();
        CExpression::Simple(format!(
            "jophet_py_flatten_or_panic({}, \"{}\", {})",
            compiled_obj_expr, filename, line
        ))
    }

    /// Helper for `compile_python_ffi_method_call`: handles `.length()`.
    /// This now calls a dedicated C runtime helper function.
    fn compile_py_length_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: String,
    ) -> CExpression {
        self.python_runtime_needed = true;
        let line = self.source_map.line_for_byte(object.span.start);
        let filename = self.source_map.filename();
        CExpression::Simple(format!(
            "jophet_py_len_or_panic({}, \"{}\", {})",
            compiled_obj_expr, filename, line
        ))
    }

    /// Helper for `compile_python_ffi_method_call`: handles `__getitem__`.
    fn compile_py_getitem_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: String,
        args: &[TypedExpression],
    ) -> CExpression {
        self.python_runtime_needed = true;
        // Use ensure_lvalue to handle temporary keys cleanly.
        let key_lvalue = self.ensure_lvalue(&args[0]);
        let key_type = &args[0].jophet_type;
        let key_ptr = format!("(void*)&{}", key_lvalue);
        let key_type_tag = self.jophet_type_to_c_enum_tag(key_type);

        // Append the source location info for the runtime panic helper.
        let line = self.source_map.line_for_byte(object.span.start);
        let filename = self.source_map.filename();

        CExpression::Simple(format!(
            "jophet_py_get_item({}, {}, {}, \"{}\", {})",
            compiled_obj_expr, key_ptr, key_type_tag, filename, line
        ))
    }

    /// Helper for `compile_python_ffi_method_call`: handles `__getattr__`.
    fn compile_py_getattr_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: String,
        args: &[TypedExpression],
    ) -> CExpression {
        self.python_runtime_needed = true;
        // The attribute name is passed as a string literal.
        let compiled_attr_name = self.compile_expression(&args[0]);
        let line = self.source_map.line_for_byte(object.span.start);
        let filename = self.source_map.filename();

        CExpression::Simple(format!(
            "jophet_py_get_attr({}, {}, \"{}\", {})",
            compiled_obj_expr, compiled_attr_name, filename, line
        ))
    }

    /// Helper for `compile_python_ffi_method_call`: handles generic method calls.
    fn compile_py_generic_method_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: String,
        mangled_name: &str,
        args: &[TypedExpression],
    ) -> CExpression {
        self.python_runtime_needed = true;
        let mut final_args = vec![
            compiled_obj_expr,
            format!("\"{}\"", mangled_name),
            format!("{}", args.len()),
        ];

        for arg in args {
            let (type_tag, ffi_arg) =
                if let JophetType::Array { member_type, size } = &arg.jophet_type {
                    // This block now handles both 1D and 2D arrays by converting them to temporary vectors.
                    let compiled_arg = self.compile_expression(arg);
                    if let JophetType::Array {
                        member_type: inner_member_type,
                        size: inner_size,
                    } = member_type.as_ref()
                    {
                        // --- 2D Array to Vector<Vector<T>> ---
                        let temp_vec_var = format!("__py_arg_vec_{}", self.temp_var_counter);
                        self.temp_var_counter += 1;

                        writeln!(
                            &mut self.output,
                            "\tJophetVector {} = Vector_new(sizeof(JophetVector));",
                            temp_vec_var
                        )
                        .unwrap();
                        let loop_var = format!("i_{}", self.temp_var_counter);
                        self.temp_var_counter += 1;

                        writeln!(
                            &mut self.output,
                            "\tfor (size_t {i} = 0; {i} < {outer_size}; ++{i}) {{",
                            i = loop_var,
                            outer_size = size
                        )
                        .unwrap();
                        let c_inner_member_type =
                            self.jophet_type_to_c_string(inner_member_type);
                        writeln!(&mut self.output, "\t\tJophetVector inner_vec = Vector_new_from_array(sizeof({}), {}[{}], {});", c_inner_member_type, compiled_arg, loop_var, inner_size).unwrap();
                        writeln!(
                            &mut self.output,
                            "\t\tVector_push(&{}, &inner_vec);",
                            temp_vec_var
                        )
                        .unwrap();
                        writeln!(&mut self.output, "\t}}").unwrap();

                        let inner_vector_type = JophetType::Vector(inner_member_type.clone());
                        let helper_name =
                            self.get_or_create_vector_deep_delete_helper(&inner_vector_type);
                        let cleanup = format!("{}(&{});", helper_name, temp_vec_var);
                        self.scope_cleanup_stack.last_mut().unwrap().push(cleanup);

                        let tag = self.jophet_type_to_c_enum_tag(&JophetType::Vector(Box::new(
                            inner_vector_type,
                        )));
                        (tag, format!("(void*)&{}", temp_vec_var))
                    } else {
                        // --- 1D Array to Vector<T> ---
                        let temp_vec_var = format!("__py_arg_vec_{}", self.temp_var_counter);
                        self.temp_var_counter += 1;
                        let c_member_type = self.jophet_type_to_c_string(member_type);

                        writeln!(&mut self.output, "\tJophetVector {} = Vector_new_from_array(sizeof({}), {}, {});", temp_vec_var, c_member_type, compiled_arg, size).unwrap();

                        // Schedule the temporary vector for cleanup.
                        let cleanup = format!("Vector_delete(&{});", temp_vec_var);
                        self.scope_cleanup_stack.last_mut().unwrap().push(cleanup);

                        let tag =
                            self.jophet_type_to_c_enum_tag(&JophetType::Vector(member_type.clone()));
                        (tag, format!("(void*)&{}", temp_vec_var))
                    }
                } else if let JophetType::Tuple(element_types) = &arg.jophet_type {
                    // Generate the helper and register the type for the dispatcher.
                    self.get_or_create_tuple_to_py_tuple_helper(element_types);
                    self.python_convertible_types
                        .insert(arg.jophet_type.clone());
                    let tag = self.jophet_type_to_c_enum_tag(&arg.jophet_type);
                    // Pass a pointer to the actual data, creating a temporary if needed.
                    let lvalue = self.ensure_lvalue(arg);
                    (tag, format!("(void*)&{}", lvalue))
                } else if let JophetType::Struct { name, .. } = &arg.jophet_type {
                    // Generate the helper and register the type.
                    self.get_or_create_struct_to_py_dict_helper(name);
                    self.python_convertible_types
                        .insert(arg.jophet_type.clone());
                    let tag = self.jophet_type_to_c_enum_tag(&arg.jophet_type);
                    let lvalue = self.ensure_lvalue(arg);
                    (tag, format!("(void*)&{}", lvalue))
                } else if let JophetType::Dictionary { key, value } = &arg.jophet_type {
                    self.get_or_create_dictionary_to_py_dict_helper(key, value);
                    self.python_convertible_types
                        .insert(arg.jophet_type.clone());
                    let tag = self.jophet_type_to_c_enum_tag(&arg.jophet_type);
                    let lvalue = self.ensure_lvalue(arg);
                    (tag, format!("(void*)&{}", lvalue))
                } else if let JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } =
                    &arg.jophet_type
                {
                    self.get_or_create_tagged_union_to_py_dict_helper(name);
                    self.python_convertible_types
                        .insert(arg.jophet_type.clone());
                    let tag = self.jophet_type_to_c_enum_tag(&arg.jophet_type);
                    let lvalue = self.ensure_lvalue(arg);
                    (tag, format!("(void*)&{}", lvalue))
                } else {
                    // For ALL types (primitive and aggregate), create a temporary variable if needed
                    // and pass a pointer to it. This makes the FFI contract uniform.
                    let type_tag = self.jophet_type_to_c_enum_tag(&arg.jophet_type);
                    let lvalue = self.ensure_lvalue(arg);
                    (type_tag, format!("(void*)&{}", lvalue))
                };

            final_args.push(ffi_arg);
            final_args.push(type_tag);
        }

        // Append the source location info for the runtime panic helper.
        final_args.push(format!("\"{}\"", self.source_map.filename()));
        final_args.push(format!(
            "{}",
            self.source_map.line_for_byte(object.span.start)
        ));

        CExpression::Simple(format!("jophet_py_call_method({})", final_args.join(", ")))
    }

    /// Attempts to compile a method call as one of Jophet's built-in methods.
    ///
    /// This has been refactored to delegate to smaller, more focused helper functions.
    /// If the `mangled_name` does not correspond to a known built-in, it returns `None`,
    /// allowing the caller to treat it as a user-defined method.
    fn try_compile_builtin_method_call(
        &mut self,
        object: &TypedExpression,
        compiled_obj_expr: &str,
        mangled_name: &str,
        args: &[TypedExpression],
        return_type: &JophetType,
    ) -> Option<CExpression> {
        match mangled_name {
            "Vector_isEmpty" | "String_isEmpty" | "String_pop" | "String_first" | "String_last" => {
                self.compile_simple_receiver_method(object, mangled_name)
            }
            "Vector_first" | "Vector_last" => {
                self.compile_vector_peek_method(object, mangled_name, return_type)
            }
            "Vector_pop" => self.compile_vector_pop_method(object, return_type),
            "Vector_contains" | "String_contains" => {
                self.compile_contains_method(object, mangled_name, args)
            }
            "map" => self.compile_map_method(object, args, return_type),
            "flatten" => self.compile_flatten_method(object, return_type),
            "eachIndex" | "mutateEach" => {
                self.compile_iterative_method(object, mangled_name, args)
            }
            "length" => self.compile_length_method(object, compiled_obj_expr),
            "set" => self.compile_dictionary_set_method(object, compiled_obj_expr, args),
            "String_get" => self.compile_string_get_method(object, args),
            "jophet_char_is_alphanumeric" | "jophet_char_is_alphabetic"
            | "jophet_char_is_digit" | "jophet_char_is_whitespace" => {
                Some(CExpression::Simple(format!(
                    "{}({})",
                    mangled_name, compiled_obj_expr
                )))
            }
            "get" => self.compile_dictionary_get_method(object, args),
            "push" => {
                // This method is handled as a statement in `compile_statement_common` to correctly
                // manage temporary variables for r-value arguments. It should not be called as
                // a simple expression. Returning `(void)0` is a safe fallback.
                Some(CExpression::Simple("(void)0".to_string()))
            }
            "String_characters" => self.compile_simple_receiver_method(object, mangled_name),
            "unchecked" => {
                let array_expr = self.ensure_lvalue(object);
                let index_expr = self.compile_expression(&args[0]);
                let result = format!("({})[{}]", array_expr, index_expr);
                Some(CExpression::Simple(result))
            }
            _ => None, // Not a built-in method
        }
    }
    
    /// Helper for `try_compile_builtin_method_call`: compiles simple methods that just take a pointer to the receiver.
    fn compile_simple_receiver_method(&mut self, object: &TypedExpression, mangled_name: &str) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        Some(CExpression::Simple(format!("{}(&{})", mangled_name, receiver)))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.first()` and `.last()`.
    fn compile_vector_peek_method(&mut self, object: &TypedExpression, mangled_name: &str, return_type: &JophetType) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let member_type = if let JophetType::Vector(m) = &object.jophet_type { m } else { unreachable!("Type mismatch: expected Vector.") };
        let (impl_fn_name, base_name) = if mangled_name == "Vector_first" { ("Vector_first_impl", "first") } else { ("Vector_last_impl", "last") };
        let wrapper_name = self.get_or_create_vector_result_wrapper(base_name, impl_fn_name, member_type, return_type, true);
        Some(CExpression::Simple(format!("{}(&{})", wrapper_name, receiver)))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.pop()`.
    fn compile_vector_pop_method(&mut self, object: &TypedExpression, return_type: &JophetType) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let member_type = if let JophetType::Vector(m) = &object.jophet_type { m } else { unreachable!("Type mismatch: expected Vector.") };
        let wrapper_name = self.get_or_create_vector_result_wrapper("pop", "Vector_pop_impl", member_type, return_type, false);
        Some(CExpression::Simple(format!("{}(&{})", wrapper_name, receiver)))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.contains()`.
    fn compile_contains_method(&mut self, object: &TypedExpression, mangled_name: &str, args: &[TypedExpression]) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let arg_lvalue = self.ensure_lvalue(&args[0]);
        let result = format!("{}(&{}, &{})", mangled_name, receiver, arg_lvalue);
        Some(CExpression::Simple(result))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.map()`.
    fn compile_map_method(&mut self, object: &TypedExpression, args: &[TypedExpression], return_type: &JophetType) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let compiled_closure = self.compile_expression(&args[0]);
        let (source_member_type, result_member_type) = match (&object.jophet_type, return_type) {
            (JophetType::Vector(s), JophetType::Vector(r)) => (s.as_ref(), r.as_ref()),
            (JophetType::Array { member_type: s, .. }, JophetType::Vector(r)) => (s.as_ref(), r.as_ref()),
            _ => unreachable!("Semantic analysis ensures map is on Vector/Array and returns Vector"),
        };
        let helper_name = self.get_or_create_map_helper(&object.jophet_type, source_member_type, result_member_type, &args[0].jophet_type);
        Some(CExpression::Simple(format!("{}(&{}, {})", helper_name, receiver, compiled_closure)))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.flatten()`.
    fn compile_flatten_method(&mut self, object: &TypedExpression, return_type: &JophetType) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let (outer_collection_type, inner_member_type) = match &object.jophet_type {
            JophetType::Vector(outer) => {
                if let JophetType::Vector(inner) = outer.as_ref() { (&object.jophet_type, inner.as_ref()) } else { unreachable!("Flatten on non-nested vector.") }
            }
            JophetType::Array { member_type: outer, .. } => {
                if let JophetType::Array { member_type: inner, .. } = outer.as_ref() { (&object.jophet_type, inner.as_ref()) } else { unreachable!("Flatten on non-nested array.") }
            }
            _ => unreachable!(),
        };
        let helper_name = self.get_or_create_flatten_helper(outer_collection_type, inner_member_type, return_type);
        Some(CExpression::Simple(format!("{}(&{})", helper_name, receiver)))
    }
    
    /// Helper for `try_compile_builtin_method_call`: handles iterative methods like `eachIndex`.
    fn compile_iterative_method(&mut self, object: &TypedExpression, mangled_name: &str, args: &[TypedExpression]) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let compiled_closure = self.compile_expression(&args[0]);
        self.generate_iterative_method_loop(&receiver, &object.jophet_type, mangled_name, &args[0], &compiled_closure);
        Some(CExpression::Simple("(void)0".to_string()))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.length()`.
    fn compile_length_method(&mut self, object: &TypedExpression, compiled_obj_expr: &str) -> Option<CExpression> {
        let result = match &object.jophet_type {
            JophetType::Vector(_) | JophetType::String | JophetType::Dictionary { .. } => format!("{}.len", compiled_obj_expr),
            JophetType::StringSlice => format!("strlen({})", compiled_obj_expr),
            _ => unreachable!("Semantic analyzer should prevent .length() on invalid types"),
        };
        Some(CExpression::Simple(result))
    }
    
    /// Helper for `try_compile_builtin_method_call`: compiles `.set()` on a Dictionary.
    fn compile_dictionary_set_method(&mut self, _object: &TypedExpression, compiled_obj_expr: &str, args: &[TypedExpression]) -> Option<CExpression> {
        self.runtime_needed = true;
        let key_lvalue = self.ensure_lvalue(&args[0]);
        let value_lvalue = self.ensure_lvalue(&args[1]);
        writeln!(&mut self.output, "\tDictionary_set(&{}, &{}, &{});", compiled_obj_expr, key_lvalue, value_lvalue).unwrap();
        Some(CExpression::Simple("".to_string()))
    }
    
    /// Helper for `try_compile_builtin_method_call`: compiles `.get()` on a String.
    fn compile_string_get_method(&mut self, object: &TypedExpression, args: &[TypedExpression]) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let compiled_index = self.compile_expression(&args[0]);
        Some(CExpression::Simple(format!("String_get(&{}, {})", receiver, compiled_index)))
    }

    /// Helper for `try_compile_builtin_method_call`: compiles `.get()` on a Dictionary.
    fn compile_dictionary_get_method(&mut self, object: &TypedExpression, args: &[TypedExpression]) -> Option<CExpression> {
        let receiver = self.ensure_lvalue(object);
        let result_var = format!("__dict_get_res_{}", self.temp_var_counter);
        self.temp_var_counter += 1;
        let key_lvalue = self.ensure_lvalue(&args[0]);

        writeln!(&mut self.output, "\tResult_void_ptr_void {} = Dictionary_get(&{}, &{});", result_var, receiver, key_lvalue).unwrap();

        Some(CExpression::Temporary(result_var))
    }

    /// A generic helper to get or create C helper functions for vector methods like `.pop()`, `.first()`, and `.last()`.
    /// This function replaces `get_or_create_vector_pop_wrapper` and `get_or_create_vector_peek_wrapper` to reduce code duplication.
    /// It generates a type-safe C wrapper around a generic `_impl` runtime function.
    pub fn get_or_create_vector_result_wrapper(
        &mut self,
        base_name: &str, // e.g., "pop", "first"
        impl_fn_name: &str, // e.g., "Vector_pop_impl"
        member_type: &JophetType,
        result_type: &JophetType,
        is_const_receiver: bool,
    ) -> String {
        let c_result_type = self.jophet_type_to_c_string(result_type);
        let mangled_member_type = self.jophet_type_to_c_string_for_mangling(member_type);

        let wrapper_name = format!("__jophet_vector_{}_{}", base_name, mangled_member_type);
        
        let receiver_param = if is_const_receiver { "const JophetVector* v" } else { "JophetVector* v" };
        let proto = format!("{} {}({});", c_result_type, wrapper_name, receiver_param);
        
        if self.function_prototypes.contains(&proto) {
            return wrapper_name;
        }

        self.function_prototypes.insert(proto);

        let mut def = String::new();
        writeln!(&mut def, "{} {}({}) {{", c_result_type, wrapper_name, receiver_param).unwrap();
        writeln!(&mut def, "\t{} res;", c_result_type).unwrap();
        // The safe C function returns a bool and copies the result into a destination.
        // The destination is the `.data.ok` field of our result struct.
        writeln!(&mut def, "\tif ({}(v, &res.data.ok)) {{", impl_fn_name).unwrap();
        writeln!(&mut def, "\t\tres.is_ok = true;").unwrap();
        writeln!(&mut def, "\t}} else {{").unwrap();
        writeln!(&mut def, "\t\tres.is_ok = false;").unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(&mut def, "\treturn res;").unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();

        wrapper_name
    }

    /// Generates a C helper function to perform a .map() operation for specific types.
    /// This function now correctly handles memory for owned types by cleaning up the
    /// temporary `new_element` variable after it has been pushed into the result vector,
    /// preventing a memory leak in each iteration of the loop.
    pub fn get_or_create_map_helper(
        &mut self,
        source_collection_type: &JophetType,
        source_member_type: &JophetType,
        result_member_type: &JophetType,
        closure_type: &JophetType,
    ) -> String {
        let mangled_source_type = self.jophet_type_to_c_string_for_mangling(source_member_type);
        let mangled_result_type = self.jophet_type_to_c_string_for_mangling(result_member_type);

        let helper_name = format!(
            "__jophet_map_from_{}_to_{}",
            mangled_source_type, mangled_result_type
        );
        let c_source_type = self.jophet_type_to_c_string(source_member_type);
        let c_result_type = self.jophet_type_to_c_string(result_member_type);
        let c_closure_type = self.jophet_type_to_c_string(closure_type);

        let proto = format!(
            "JophetVector {}(const void* collection, {} closure);",
            helper_name, c_closure_type
        );
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }
        self.function_prototypes.insert(proto);

        let (size_expr, element_access_expr) = match source_collection_type {
            JophetType::Array { size, .. } => (
                size.to_string(),
                format!("((const {}*)collection)[i]", c_source_type),
            ),
            JophetType::Vector(_) => (
                "((const JophetVector*)collection)->len".to_string(),
                format!(
                    "(({}*)((const JophetVector*)collection)->data)[i]",
                    c_source_type
                ),
            ),
            _ => unreachable!("Semantic analysis ensures `map` is only called on valid collection types."),
        };

        // Create a fake untyped expression to pass to the call compiler
        let fake_callable_expr = TypedExpression {
            kind: TypedExpressionKind::Identifier {
                name: "closure".to_string(),
                mangled_name: None,
            },
            jophet_type: closure_type.clone(),
            span: Default::default(),
        };
        let fake_arg_expr = TypedExpression {
            kind: TypedExpressionKind::Identifier {
                name: "current_element".to_string(),
                mangled_name: None,
            },
            jophet_type: source_member_type.clone(),
            span: Default::default(),
        };
        let (closure_params, closure_ret) =
            if let JophetType::Closure { params, ret, .. } = closure_type {
                (params, ret)
            } else {
                unreachable!("Expected a closure type for the map operation.")
            };
        let closure_call = self
            .compile_function_call_expression(
                &TypedCallKind::Closure {
                    callable_expr: Box::new(fake_callable_expr),
                    params: closure_params.clone(),
                    ret: closure_ret.clone(),
                },
                &[fake_arg_expr],
                // This last argument is a dummy, it's not used in closure call compilation
                &TypedExpression {
                    kind: TypedExpressionKind::Identifier {
                        name: "dummy".to_string(),
                        mangled_name: None,
                    },
                    jophet_type: JophetType::Nothing,
                    span: Default::default(),
                },
            )
            .into_string();

        let cleanup_call = self.get_cleanup_call(result_member_type, "new_element", false);

        let mut def = String::new();
        writeln!(
            &mut def,
            "JophetVector {}(const void* collection, {} closure) {{",
            helper_name, c_closure_type
        )
        .unwrap();
        writeln!(&mut def, "\tJophetVector result = Vector_new(sizeof({}));", c_result_type)
            .unwrap();
        writeln!(&mut def, "\tfor (size_t i = 0; i < {}; ++i) {{", size_expr).unwrap();
        writeln!(
            &mut def,
            "\t\t{} current_element = {};",
            c_source_type, element_access_expr
        )
        .unwrap();
        writeln!(&mut def, "\t\t{} new_element = {};", c_result_type, closure_call).unwrap();
        writeln!(&mut def, "\t\tVector_push(&result, &new_element);").unwrap();
        // If the created element is an owned type, we must clean up the temporary
        // `new_element` variable after its contents have been copied into the vector.
        if !cleanup_call.is_empty() {
            writeln!(&mut def, "\t\t{};", cleanup_call).unwrap();
        }
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(&mut def, "\tJophetClosure_delete(&closure);").unwrap();
        writeln!(&mut def, "\treturn result;").unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Generates a C helper function to perform a `.flatten()` operation for specific nested collection types.
    ///
    /// This function handles flattening both `Array<Array<T>>` and `Vector<Vector<T>>` into a new `Vector<T>`.
    /// It ensures that if `T` is an owned type, elements are correctly cloned into the new flat vector.
    /// It achieves genericity by generating small, static C thunk functions for accessing the inner
    /// collection's length and data, and passing these function pointers to a generic runtime implementation.
    pub fn get_or_create_flatten_helper(
        &mut self,
        outer_collection_type: &JophetType,
        inner_member_type: &JophetType,
        result_type: &JophetType,
    ) -> String {
        let mangled_source_type = self.jophet_type_to_c_string_for_mangling(outer_collection_type);
        let helper_name = format!("__jophet_flatten_from_{}", mangled_source_type);
        let c_result_type = self.jophet_type_to_c_string(result_type);
    
        let proto = format!("{} {}(const void* collection);", c_result_type, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }
        self.function_prototypes.insert(proto);
        self.runtime_needed = true;
    
        let c_inner_member_type = self.jophet_type_to_c_string(inner_member_type);
    
        // Thunks required by jophet_flatten_impl
        let inner_len_fn_name = format!("{}_inner_len", helper_name);
        let inner_data_fn_name = format!("{}_inner_data", helper_name);
        let clone_fn_arg = if self.type_is_cloneable(inner_member_type) {
            format!("&{}", self.get_or_create_item_clone_thunk(inner_member_type))
        } else {
            "NULL".to_string()
        };
        
        let mut def = String::new();
    
        // Generate the specific thunks for this collection type
        match outer_collection_type {
            JophetType::Array { member_type: outer_member, size: outer_size } => {
                let (inner_c_type, inner_size) = if let JophetType::Array { size, .. } = outer_member.as_ref() {
                    (self.jophet_type_to_c_string(outer_member), *size)
                } else { unreachable!("Semantic analysis ensures `flatten` is called on a nested Array.") };
                
                // len_fn: returns constant size for inner array
                writeln!(&mut def, "static size_t {}(const void* inner_coll) {{ (void)inner_coll; return {}; }}", inner_len_fn_name, inner_size).unwrap();
                
                // data_fn: returns pointer to inner array data, or size of inner array type
                writeln!(&mut def, "static const void* {}(const void* inner_coll) {{ if (inner_coll == NULL) {{ return (const void*)sizeof({}); }} return (const void*)inner_coll; }}", inner_data_fn_name, inner_c_type).unwrap();
                
                // main wrapper function
                writeln!(&mut def, "{} {}(const void* collection) {{", c_result_type, helper_name).unwrap();
                writeln!(&mut def, "\treturn jophet_flatten_impl(collection, {}, &{}, sizeof({}), &{}, {});", outer_size, inner_len_fn_name, c_inner_member_type, inner_data_fn_name, clone_fn_arg).unwrap();
                writeln!(&mut def, "}}").unwrap();
            },
            JophetType::Vector(outer_member) => {
                let inner_c_type = self.jophet_type_to_c_string(outer_member);
                
                // len_fn: returns .len for inner vector
                writeln!(&mut def, "static size_t {}(const void* inner_coll) {{ return ((const JophetVector*)inner_coll)->len; }}", inner_len_fn_name).unwrap();
                
                // data_fn: returns .data for inner vector, or size of inner vector type
                writeln!(&mut def, "static const void* {}(const void* inner_coll) {{ if (inner_coll == NULL) {{ return (const void*)sizeof({}); }} return ((const JophetVector*)inner_coll)->data; }}", inner_data_fn_name, inner_c_type).unwrap();
                
                // main wrapper function
                writeln!(&mut def, "{} {}(const void* collection) {{", c_result_type, helper_name).unwrap();
                writeln!(&mut def, "\tconst JophetVector* outer_vec = (const JophetVector*)collection;").unwrap();
                writeln!(&mut def, "\treturn jophet_flatten_impl(outer_vec->data, outer_vec->len, &{}, sizeof({}), &{}, {});", inner_len_fn_name, c_inner_member_type, inner_data_fn_name, clone_fn_arg).unwrap();
                writeln!(&mut def, "}}").unwrap();
            },
            _ => unreachable!("Semantic analysis ensures `flatten` is only called on valid collection types."),
        };
    
        writeln!(&mut self.function_defs, "{}\n", def).unwrap();
        helper_name
    }

    /// Generates a C `for` loop for an iterative method that produces no return value.
    pub fn generate_iterative_method_loop(
        &mut self,
        receiver_name: &str,
        collection_type: &JophetType,
        method_name: &str,
        closure_expr: &TypedExpression,
        compiled_closure: &str,
    ) {
        let (member_type, size_expr) = match collection_type {
            JophetType::Vector(m) => (m.as_ref(), format!("{}.len", receiver_name)),
            JophetType::Array { member_type, size } => (member_type.as_ref(), size.to_string()),
            _ => unreachable!("Iterative methods must be on Vector or Array."),
        };

        let index_var = format!("__iter_idx_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        writeln!(&mut self.output, "{{").unwrap();
        writeln!(
            &mut self.output,
            "\tfor (size_t {} = 0; {} < {}; ++{}) {{",
            index_var, index_var, size_expr, index_var
        )
        .unwrap();

        let (params, ret) = if let JophetType::Closure { params, ret, .. } = &closure_expr.jophet_type {
            (params, ret)
        } else {
            unreachable!("Iterative method argument must be a closure.")
        };
        let fake_callable_expr = TypedExpression {
            kind: TypedExpressionKind::Identifier {
                name: compiled_closure.to_string(),
                mangled_name: None,
            },
            jophet_type: closure_expr.jophet_type.clone(),
            span: Default::default(),
        };
        let dummy_return_expr = TypedExpression {
            kind: TypedExpressionKind::Identifier {
                name: "dummy".to_string(),
                mangled_name: None,
            },
            jophet_type: JophetType::Nothing,
            span: Default::default(),
        };

        let closure_call = match method_name {
            "eachIndex" => {
                let fake_arg_expr = TypedExpression { kind: TypedExpressionKind::Identifier { name: index_var.clone(), mangled_name: None }, jophet_type: JophetType::UInt(64), span: Default::default() };
                self.compile_function_call_expression(&TypedCallKind::Closure { callable_expr: Box::new(fake_callable_expr), params: params.clone(), ret: ret.clone() }, &[fake_arg_expr], &dummy_return_expr).into_string()
            },
            "mutateEach" => {
                let c_member_type = self.jophet_type_to_c_string(member_type);
                let element_ptr_expr_str = match collection_type {
                    JophetType::Array { .. } => format!("&({}[{}])", receiver_name, index_var),
                    JophetType::Vector(_) => format!("&((({}*){}.data)[{}])", c_member_type, receiver_name, index_var),
                    _ => unreachable!(),
                };
                let fake_arg_expr = TypedExpression { kind: TypedExpressionKind::Identifier { name: element_ptr_expr_str, mangled_name: None }, jophet_type: JophetType::MutableReference(Box::new(member_type.clone())), span: Default::default() };
                self.compile_function_call_expression(&TypedCallKind::Closure { callable_expr: Box::new(fake_callable_expr), params: params.clone(), ret: ret.clone() }, &[fake_arg_expr], &dummy_return_expr).into_string()
            },
            _ => unreachable!("Unknown iterative method: {}", method_name),
        };

        writeln!(&mut self.output, "\t\t{};", closure_call).unwrap();
        writeln!(&mut self.output, "\t}}").unwrap();
        writeln!(
            &mut self.output,
            "\tJophetClosure_delete(&{});",
            compiled_closure
        )
        .unwrap();
        writeln!(&mut self.output, "}}").unwrap();
    }

    /// Gets or creates a C helper function to convert a specific Jophet tuple type to a Python tuple.
    /// It now correctly generates code that passes a pointer to each tuple member, including
    /// primitives, to the generic `jophet_to_py_object` runtime converter, which expects a pointer.
    pub fn get_or_create_tuple_to_py_tuple_helper(
        &mut self,
        element_types: &[JophetType],
    ) -> String {
        let tuple_type = JophetType::Tuple(element_types.to_vec());
        let c_tuple_type = self.jophet_type_to_c_string(&tuple_type);
        let mangled_tuple_type = self.jophet_type_to_c_string_for_mangling(&tuple_type);

        let helper_name = format!("__jophet_tuple_to_py_tuple_{}", mangled_tuple_type);

        let proto = format!("static PyObject* {}(const void* data);", helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        self.function_prototypes.insert(proto);

        let mut def = String::new();
        writeln!(&mut def, "static PyObject* {}(const void* data) {{", helper_name).unwrap();
        writeln!(
            &mut def,
            "\tconst {}* t = (const {}*)data;",
            c_tuple_type, c_tuple_type
        )
        .unwrap();
        writeln!(
            &mut def,
            "\tPyObject* pTuple = PyTuple_New({});",
            element_types.len()
        )
        .unwrap();
        writeln!(&mut def, "\tif (!pTuple) return NULL;").unwrap();

        for (i, element_type) in element_types.iter().enumerate() {
            let field_access = format!("t->f{}", i);
            let element_tag = self.jophet_type_to_c_enum_tag(element_type);

            // Construct the data pointer for the jophet_to_py_object call.
            // This MUST be a pointer to the data for ALL types, as the runtime function
            // expects to receive a `const void*` and dereference it.
            let data_ptr_expr = format!("(const void*)&{}", field_access);

            writeln!(
                &mut def,
                "\tPyObject* pItem_{i} = jophet_to_py_object({data_ptr}, {tag});",
                i = i,
                data_ptr = data_ptr_expr,
                tag = element_tag
            )
            .unwrap();

            writeln!(&mut def, "\tif (!pItem_{}) {{", i).unwrap();
            writeln!(&mut def, "\t\tPy_DECREF(pTuple);").unwrap();
            writeln!(&mut def, "\t\treturn NULL;").unwrap();
            writeln!(&mut def, "\t}}").unwrap();
            writeln!(&mut def, "\tPyTuple_SetItem(pTuple, {}, pItem_{});", i, i).unwrap();
        }

        writeln!(&mut def, "\treturn pTuple;").unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a specific Jophet struct type to a Python dictionary.
    /// It now correctly generates code that passes a pointer to each struct field, including
    /// primitives, to the generic `jophet_to_py_object` runtime converter, which expects a pointer.
    pub fn get_or_create_struct_to_py_dict_helper(&mut self, struct_name: &str) -> String {
        let mangled_struct_name = struct_name.replace('*', "ptr").replace(' ', "_");
        let helper_name = format!("__jophet_struct_to_py_dict_{}", mangled_struct_name);

        let proto = format!("static PyObject* {}(const void* data);", helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        let struct_def = self
            .struct_defs_cache
            .get(struct_name)
            .unwrap_or_else(|| panic!("Struct definition for '{}' not found in cache", struct_name));

        self.function_prototypes.insert(proto);

        let mut def = String::new();
        writeln!(&mut def, "static PyObject* {}(const void* data) {{", helper_name).unwrap();
        writeln!(
            &mut def,
            "\tconst {}* s = (const {}*)data;",
            struct_name, struct_name
        )
        .unwrap();
        writeln!(&mut def, "\tPyObject* pDict = PyDict_New();").unwrap();
        writeln!(&mut def, "\tif (!pDict) return NULL;").unwrap();

        for (field_name, field_type, _) in &struct_def.fields {
            let sanitized_field_name = self.sanitize_c_keyword(field_name);
            let field_access = format!("s->{}", sanitized_field_name);
            let element_tag = self.jophet_type_to_c_enum_tag(field_type);

            // This MUST be a pointer to the data for ALL types, as the runtime function
            // expects to receive a `const void*` and dereference it.
            let data_ptr_expr = format!("(const void*)&{}", field_access);

            writeln!(
                &mut def,
                "\tPyObject* pValue_{name} = jophet_to_py_object({data_ptr}, {tag});",
                name = sanitized_field_name,
                data_ptr = data_ptr_expr,
                tag = element_tag
            )
            .unwrap();

            writeln!(&mut def, "\tif (!pValue_{}) {{", sanitized_field_name).unwrap();
            writeln!(&mut def, "\t\tPy_DECREF(pDict);").unwrap();
            writeln!(&mut def, "\t\treturn NULL;").unwrap();
            writeln!(&mut def, "\t}}").unwrap();
            writeln!(
                &mut def,
                "\tif (PyDict_SetItemString(pDict, \"{name}\", pValue_{name}) < 0) {{",
                name = field_name,
            )
            .unwrap();
            writeln!(&mut def, "\t\tPy_DECREF(pDict);").unwrap();
            writeln!(&mut def, "\t\tPy_DECREF(pValue_{});", sanitized_field_name).unwrap();
            writeln!(&mut def, "\t\treturn NULL;").unwrap();
            writeln!(&mut def, "\t}}").unwrap();
            writeln!(&mut def, "\tPy_DECREF(pValue_{});", sanitized_field_name).unwrap();
        }

        writeln!(&mut def, "\treturn pDict;").unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a specific Jophet dictionary type to a Python dictionary.
    pub fn get_or_create_dictionary_to_py_dict_helper(
        &mut self,
        key_type: &JophetType,
        value_type: &JophetType,
    ) -> String {
        let mangled_key_type = self.jophet_type_to_c_string_for_mangling(key_type);
        let mangled_value_type = self.jophet_type_to_c_string_for_mangling(value_type);
        let helper_name = format!(
            "__jophet_dict_to_py_dict_{}_{}",
            mangled_key_type, mangled_value_type
        );

        let proto = format!("static PyObject* {}(const void* data);", helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }
        self.function_prototypes.insert(proto);

        let key_tag = self.jophet_type_to_c_enum_tag(key_type);
        let value_tag = self.jophet_type_to_c_enum_tag(value_type);

        let mut def = String::new();
        writeln!(&mut def, "static PyObject* {}(const void* data) {{", helper_name).unwrap();
        writeln!(&mut def, "\tconst JophetDictionary* d = (const JophetDictionary*)data;").unwrap();
        writeln!(&mut def, "\tPyObject* pDict = PyDict_New();").unwrap();
        writeln!(&mut def, "\tif (!pDict) return NULL;").unwrap();
        writeln!(&mut def, "\tfor (size_t i = 0; i < d->capacity; i++) {{").unwrap();
        writeln!(&mut def, "\t\tJophetDictionaryEntry* entry = d->buckets[i];").unwrap();
        writeln!(&mut def, "\t\twhile (entry) {{").unwrap();
        writeln!(&mut def, "\t\t\tPyObject* pKey = jophet_to_py_object(entry->key, {});", key_tag).unwrap();
        writeln!(&mut def, "\t\t\tPyObject* pValue = jophet_to_py_object(entry->value, {});", value_tag).unwrap();
        writeln!(&mut def, "\t\t\tif (!pKey || !pValue) {{ Py_XDECREF(pKey); Py_XDECREF(pValue); Py_DECREF(pDict); return NULL; }}").unwrap();
        writeln!(&mut def, "\t\t\tif (PyDict_SetItem(pDict, pKey, pValue) < 0) {{ Py_DECREF(pKey); Py_DECREF(pValue); Py_DECREF(pDict); return NULL; }}").unwrap();
        writeln!(&mut def, "\t\t\tPy_DECREF(pKey); Py_DECREF(pValue);").unwrap();
        writeln!(&mut def, "\t\t\tentry = entry->next;").unwrap();
        writeln!(&mut def, "\t\t}}").unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(&mut def, "\treturn pDict;").unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a specific Jophet TaggedUnion or Error type to a Python dictionary.
    /// It now correctly generates code that passes a pointer to the variant's payload, including
    /// primitives, to the generic `jophet_to_py_object` runtime converter, which expects a pointer.
    pub fn get_or_create_tagged_union_to_py_dict_helper(&mut self, type_name: &str) -> String {
        let mangled_type_name = type_name.replace('*', "ptr").replace(' ', "_");
        let helper_name = format!("__jophet_tagged_union_to_py_dict_{}", mangled_type_name);

        let proto = format!("static PyObject* {}(const void* data);", helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        let owned_def: TypedTaggedUnionDef;
        let def = if let Some(def) = self.tagged_union_defs_cache.get(type_name) {
            def
        } else if let Some(err_def) = self.error_defs_cache.get(type_name) {
            owned_def = TypedTaggedUnionDef {
                is_public: err_def.is_public,
                name: err_def.name.clone(),
                doc_comment: err_def.doc_comment.clone(),
                generic_params: Vec::new(),
                variants: err_def.variants.clone(),
                module_path: err_def.module_path.clone(),
            };
            &owned_def
        } else {
            panic!("Definition for '{}' not found in cache", type_name);
        };

        self.function_prototypes.insert(proto);

        let mut func_def = String::new();
        writeln!(&mut func_def, "static PyObject* {}(const void* data) {{", helper_name).unwrap();
        writeln!(&mut func_def, "\tconst {}* s = (const {}*)data;", type_name, type_name).unwrap();
        writeln!(&mut func_def, "\tPyObject* pDict = PyDict_New();").unwrap();
        writeln!(&mut func_def, "\tif (!pDict) return NULL;").unwrap();
        
        writeln!(&mut func_def, "\tswitch (s->tag) {{").unwrap();
        for variant in &def.variants {
            let full_variant_name = format!("{}_{}", type_name, variant.name);
            writeln!(&mut func_def, "\t\tcase {}: {{", full_variant_name).unwrap();
            writeln!(&mut func_def, "\t\t\tPyObject* pTag = PyUnicode_FromString(\"{}\");", variant.name).unwrap();
            writeln!(&mut func_def, "\t\t\tPyDict_SetItemString(pDict, \"tag\", pTag);").unwrap();
            writeln!(&mut func_def, "\t\t\tPy_DECREF(pTag);").unwrap();

            if let Some(payload_type) = &variant.payload {
                let element_tag = self.jophet_type_to_c_enum_tag(payload_type);
                let field_access = format!("s->data.{}", variant.name);
                // This MUST be a pointer to the data for ALL types, as the runtime function
                // expects to receive a `const void*` and dereference it.
                let data_ptr_expr = format!("(const void*)&{}", field_access);
                writeln!(&mut func_def, "\t\t\tPyObject* pPayload = jophet_to_py_object({}, {});", data_ptr_expr, element_tag).unwrap();
                writeln!(&mut func_def, "\t\t\tPyDict_SetItemString(pDict, \"payload\", pPayload);").unwrap();
                writeln!(&mut func_def, "\t\t\tPy_DECREF(pPayload);").unwrap();
            } else {
                writeln!(&mut func_def, "\t\t\tPy_INCREF(Py_None);").unwrap();
                writeln!(&mut func_def, "\t\t\tPyDict_SetItemString(pDict, \"payload\", Py_None);").unwrap();
                writeln!(&mut func_def, "\t\t\tPy_DECREF(Py_None);").unwrap();
            }

            writeln!(&mut func_def, "\t\t\tbreak;").unwrap();
            writeln!(&mut func_def, "\t\t}}").unwrap();
        }
        writeln!(&mut func_def, "\t}}").unwrap();
        
        writeln!(&mut func_def, "\treturn pDict;").unwrap();
        writeln!(&mut func_def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", func_def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `int` to a specific Jophet `Enum`.
    pub fn get_or_create_py_to_enum_helper(&mut self, enum_name: &str) -> String {
        let mangled_enum_name = enum_name.replace('*', "ptr").replace(' ', "_");
        let helper_name = format!("__jophet_py_to_enum_{}", mangled_enum_name);

        let proto = format!("Result_{}_FfiError {}(PythonObject handle);", enum_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        let enum_def = self.enum_defs_cache.get(enum_name)
            .unwrap_or_else(|| panic!("Enum definition for '{}' not found in cache", enum_name));

        self.function_prototypes.insert(proto);
        let result_type_name = format!("Result_{}_FfiError", enum_name);

        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyLong_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python int.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(&mut def, "\tlong long val = PyLong_AsLongLong(handle);").unwrap();
        writeln!(&mut def, "\tif (PyErr_Occurred()) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = get_python_exception_string();").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        writeln!(&mut def, "\tswitch (val) {{").unwrap();
        for (member_name, member_value, _) in &enum_def.members {
            writeln!(&mut def, "\t\tcase {}:", member_value).unwrap();
        }
        writeln!(&mut def, "\t\t\treturn ({}){{ .is_ok = true, .data.ok = ({})val }};", result_type_name, enum_name).unwrap();

        writeln!(&mut def, "\t\tdefault: {{").unwrap();
        writeln!(&mut def, "\t\t\tJophetString msg = String_new_from(\"Integer value is not a valid member of this enum.\");").unwrap();
        writeln!(&mut def, "\t\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t\t}}").unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        writeln!(&mut def, "}}").unwrap();
        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `tuple` to a specific Jophet `Tuple`.
    pub fn get_or_create_py_to_tuple_helper(&mut self, element_types: &[JophetType]) -> String {
        let tuple_type = JophetType::Tuple(element_types.to_vec());
        let c_tuple_type = self.jophet_type_to_c_string(&tuple_type);
        let mangled_tuple_type = self.jophet_type_to_c_string_for_mangling(&tuple_type);

        let helper_name = format!("__jophet_py_to_tuple_{}", mangled_tuple_type);
        let result_type_name = format!("Result_{}_FfiError", c_tuple_type.replace(' ', "_"));

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        self.function_prototypes.insert(proto);

        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyTuple_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python tuple.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(&mut def, "\tif (PyTuple_Size(handle) != {}) {{", element_types.len()).unwrap();
        writeln!(&mut def, "\t\tchar err_buf[128];").unwrap();
        writeln!(&mut def, "\t\tsnprintf(err_buf, sizeof(err_buf), \"Expected a tuple of size {}, but got size %zd.\", (size_t)PyTuple_Size(handle));", element_types.len()).unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(err_buf);").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        
        writeln!(&mut def, "\t{} t;", c_tuple_type).unwrap();
        
        for (i, element_type) in element_types.iter().enumerate() {
            let mangled_element_type = self.jophet_type_to_c_string_for_mangling(element_type);
            let conversion_fn = format!("jophet_py_convert_to_{}", mangled_element_type);
            let result_type = self.jophet_type_to_c_string(&JophetType::Fallible {
                ok: Box::new(element_type.clone()),
                err: Box::new(JophetType::Error { name: "FfiError".to_string(), module_path: PathBuf::from("std") }),
            });

            writeln!(&mut def, "\tPyObject* item_{} = PyTuple_GetItem(handle, {});", i, i).unwrap();
            writeln!(&mut def, "\t{} res_{} = {}(item_{});", result_type, i, conversion_fn, i).unwrap();
            writeln!(&mut def, "\tif (!res_{}.is_ok) {{ return ({}){{ .is_ok = false, .data.err = res_{}.data.err }}; }}", i, result_type_name, i).unwrap();
            writeln!(&mut def, "\tt.f{} = res_{}.data.ok;", i, i).unwrap();
        }

        writeln!(&mut def, "\treturn ({}){{ .is_ok = true, .data.ok = t }};", result_type_name).unwrap();
        writeln!(&mut def, "}}").unwrap();
        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `dict` to a specific Jophet `Struct`.
    pub fn get_or_create_py_to_struct_helper(&mut self, struct_name: &str) -> String {
        let mangled_struct_name = struct_name.replace('*', "ptr").replace(' ', "_");
        let helper_name = format!("__jophet_py_to_struct_{}", mangled_struct_name);
        let result_type_name = format!("Result_{}_FfiError", mangled_struct_name);

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }
        
        let fields_to_process = self
            .struct_defs_cache
            .get(struct_name)
            .unwrap_or_else(|| panic!("Struct definition for '{}' not found in cache", struct_name))
            .fields
            .clone();
        
        self.function_prototypes.insert(proto);
        
        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyDict_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python dict.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        
        writeln!(&mut def, "\t{} s;", struct_name).unwrap();
        
        for (field_name, field_type, _) in &fields_to_process {
            let mangled_field_type = self.jophet_type_to_c_string_for_mangling(field_type);
            let conversion_fn = format!("jophet_py_convert_to_{}", mangled_field_type);
             let result_type = self.jophet_type_to_c_string(&JophetType::Fallible {
                ok: Box::new(field_type.clone()),
                err: Box::new(JophetType::Error { name: "FfiError".to_string(), module_path: PathBuf::from("std") }),
            });
            let sanitized_field_name = self.sanitize_c_keyword(field_name);
            
            writeln!(&mut def, "\tPyObject* item_{} = PyDict_GetItemString(handle, \"{}\");", sanitized_field_name, field_name).unwrap();
            writeln!(&mut def, "\tif (item_{} == NULL) {{", sanitized_field_name).unwrap();
            writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Missing required key in dict for struct conversion.\");").unwrap();
            writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
            writeln!(&mut def, "\t}}").unwrap();
            writeln!(&mut def, "\t{} res_{} = {}(item_{});", result_type, sanitized_field_name, conversion_fn, sanitized_field_name).unwrap();
            writeln!(&mut def, "\tif (!res_{}.is_ok) {{ return ({}){{ .is_ok = false, .data.err = res_{}.data.err }}; }}", sanitized_field_name, result_type_name, sanitized_field_name).unwrap();
            writeln!(&mut def, "\ts.{} = res_{}.data.ok;", sanitized_field_name, sanitized_field_name).unwrap();
        }
        
        writeln!(&mut def, "\treturn ({}){{ .is_ok = true, .data.ok = s }};", result_type_name).unwrap();
        writeln!(&mut def, "}}").unwrap();
        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `dict` to a specific Jophet `TaggedUnion` or `Error`.
    pub fn get_or_create_py_to_tagged_union_helper(&mut self, type_name: &str) -> String {
        let mangled_type_name = type_name.replace('*', "ptr").replace(' ', "_");
        let helper_name = format!("__jophet_py_to_tagged_union_{}", mangled_type_name);
        let result_type_name = format!("Result_{}_FfiError", mangled_type_name);

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        let def = if let Some(def) = self.tagged_union_defs_cache.get(type_name) {
            def.clone()
        } else if let Some(err_def) = self.error_defs_cache.get(type_name) {
            TypedTaggedUnionDef {
                is_public: err_def.is_public,
                name: err_def.name.clone(),
                doc_comment: err_def.doc_comment.clone(),
                generic_params: Vec::new(),
                variants: err_def.variants.clone(),
                module_path: err_def.module_path.clone(),
            }
        } else {
            panic!("Definition for '{}' not found in cache", type_name);
        };

        self.function_prototypes.insert(proto);

        let mut func_def = String::new();
        writeln!(&mut func_def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut func_def, "\tif (!PyDict_Check(handle)) {{").unwrap();
        writeln!(&mut func_def, "\t\tJophetString msg = String_new_from(\"Object is not a Python dict.\");").unwrap();
        writeln!(&mut func_def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut func_def, "\t}}").unwrap();

        writeln!(&mut func_def, "\tPyObject* tag_obj = PyDict_GetItemString(handle, \"tag\");").unwrap();
        writeln!(&mut func_def, "\tif (tag_obj == NULL || !PyUnicode_Check(tag_obj)) {{").unwrap();
        writeln!(&mut func_def, "\t\tJophetString msg = String_new_from(\"Dict is missing a string 'tag' key.\");").unwrap();
        writeln!(&mut func_def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut func_def, "\t}}").unwrap();
        writeln!(&mut func_def, "\tconst char* tag_str = PyUnicode_AsUTF8(tag_obj);").unwrap();

        writeln!(&mut func_def, "\t{} s;", type_name).unwrap();

        for variant in &def.variants {
            writeln!(&mut func_def, "\tif (strcmp(tag_str, \"{}\") == 0) {{", variant.name).unwrap();
            writeln!(&mut func_def, "\t\ts.tag = {}_{};", type_name, variant.name).unwrap();

            if let Some(payload_type) = &variant.payload {
                 let mangled_payload_type = self.jophet_type_to_c_string_for_mangling(payload_type);
                 let conversion_fn = format!("jophet_py_convert_to_{}", mangled_payload_type);
                 let result_type = self.jophet_type_to_c_string(&JophetType::Fallible {
                    ok: Box::new(payload_type.clone()),
                    err: Box::new(JophetType::Error { name: "FfiError".to_string(), module_path: PathBuf::from("std") }),
                });

                writeln!(&mut func_def, "\t\tPyObject* payload_obj = PyDict_GetItemString(handle, \"payload\");").unwrap();
                writeln!(&mut func_def, "\t\tif (payload_obj == NULL) {{").unwrap();
                writeln!(&mut func_def, "\t\t\tJophetString msg = String_new_from(\"Dict with tag '{}' is missing a 'payload' key.\");", variant.name).unwrap();
                writeln!(&mut func_def, "\t\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
                writeln!(&mut func_def, "\t\t}}").unwrap();
                
                writeln!(&mut func_def, "\t\t{} res = {}(payload_obj);", result_type, conversion_fn).unwrap();
                writeln!(&mut func_def, "\t\tif (!res.is_ok) {{ return ({}){{ .is_ok = false, .data.err = res.data.err }}; }}", result_type_name).unwrap();
                writeln!(&mut func_def, "\t\ts.data.{} = res.data.ok;", variant.name).unwrap();

            }

            writeln!(&mut func_def, "\t\treturn ({}){{ .is_ok = true, .data.ok = s }};", result_type_name).unwrap();
            writeln!(&mut func_def, "\t}}").unwrap();
        }
        
        writeln!(&mut func_def, "\tJophetString msg = String_new_from(\"Unknown tag value for this type.\");").unwrap();
        writeln!(&mut func_def, "\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut func_def, "}}").unwrap();
        
        writeln!(&mut self.function_defs, "{}", func_def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `list` to a `Vector<PythonObject>`.
    pub fn get_or_create_py_to_vector_python_object_helper(&mut self) -> String {
        let helper_name = "__jophet_py_to_vector_python_object";
        let result_type_name = "Result_JophetVector_FfiError";

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name.to_string();
        }

        self.function_prototypes.insert(proto);

        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyList_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python list.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        writeln!(&mut def, "\tJophetVector vec = Vector_new(sizeof(PythonObject));").unwrap();
        writeln!(&mut def, "\tPy_ssize_t size = PyList_Size(handle);").unwrap();
        writeln!(&mut def, "\tfor (Py_ssize_t i = 0; i < size; i++) {{").unwrap();
        writeln!(&mut def, "\t\tPyObject* item = PyList_GetItem(handle, i);").unwrap();
        writeln!(&mut def, "\t\tPy_INCREF(item); // The vector now holds a reference").unwrap();
        writeln!(&mut def, "\t\tVector_push(&vec, &item);").unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        writeln!(&mut def, "\treturn ({}){{ .is_ok = true, .data.ok = vec }};", result_type_name).unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name.to_string()
    }

    /// Gets or creates a C helper function to convert a Python `dict` to a specific Jophet `Dictionary`.
    pub fn get_or_create_py_to_dictionary_helper(&mut self, key_type: &JophetType, value_type: &JophetType) -> String {
        let mangled_key_type = self.jophet_type_to_c_string_for_mangling(key_type);
        let mangled_value_type = self.jophet_type_to_c_string_for_mangling(value_type);

        let helper_name = format!("__jophet_py_to_dictionary_{}_{}", mangled_key_type, mangled_value_type);
        let result_type_name = format!("Result_JophetDictionary_FfiError");

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        self.function_prototypes.insert(proto);

        let c_key_type = self.jophet_type_to_c_string(key_type);
        let c_value_type = self.jophet_type_to_c_string(value_type);
        let key_conversion_fn = format!("jophet_py_convert_to_{}", mangled_key_type);
        let value_conversion_fn = format!("jophet_py_convert_to_{}", mangled_value_type);
        let key_result_type = self.jophet_type_to_c_string(&JophetType::Fallible {
            ok: Box::new(key_type.clone()),
            err: Box::new(JophetType::Error { name: "FfiError".to_string(), module_path: PathBuf::from("std") }),
        });
        let value_result_type = self.jophet_type_to_c_string(&JophetType::Fallible {
            ok: Box::new(value_type.clone()),
            err: Box::new(JophetType::Error { name: "FfiError".to_string(), module_path: PathBuf::from("std") }),
        });

        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyDict_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python dict.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        
        // Dictionary_new call needs the function pointers for deep clone/delete if keys/values are owned types
        let key_del_fn = if self.type_needs_cleanup(key_type) { format!("&{}", self.get_or_create_item_delete_thunk(key_type)) } else { "NULL".to_string() };
        let val_del_fn = if self.type_needs_cleanup(value_type) { format!("&{}", self.get_or_create_item_delete_thunk(value_type)) } else { "NULL".to_string() };
        let key_clone_fn = if self.type_is_cloneable(key_type) { format!("&{}", self.get_or_create_item_clone_thunk(key_type)) } else { "NULL".to_string() };
        let val_clone_fn = if self.type_is_cloneable(value_type) { format!("&{}", self.get_or_create_item_clone_thunk(value_type)) } else { "NULL".to_string() };

        writeln!(&mut def, "\tJophetDictionary d = Dictionary_new(sizeof({}), sizeof({}), {}, {}, {}, {});", c_key_type, c_value_type, key_del_fn, val_del_fn, key_clone_fn, val_clone_fn).unwrap();
        writeln!(&mut def, "\tPyObject *pKey, *pValue;").unwrap();
        writeln!(&mut def, "\tPy_ssize_t pos = 0;").unwrap();
        writeln!(&mut def, "\twhile (PyDict_Next(handle, &pos, &pKey, &pValue)) {{").unwrap();
        
        // Convert Python key to Jophet key
        writeln!(&mut def, "\t\t{} key_res = {}(pKey);", key_result_type, key_conversion_fn).unwrap();
        writeln!(&mut def, "\t\tif (!key_res.is_ok) {{ Dictionary_delete(&d); return ({}){{ .is_ok = false, .data.err = key_res.data.err }}; }}", result_type_name).unwrap();
        
        // Convert Python value to Jophet value
        writeln!(&mut def, "\t\t{} val_res = {}(pValue);", value_result_type, value_conversion_fn).unwrap();
        writeln!(&mut def, "\t\tif (!val_res.is_ok) {{ {} cleanup = key_res.data.ok; {}_delete(&cleanup); Dictionary_delete(&d); return ({}){{ .is_ok = false, .data.err = val_res.data.err }}; }}", c_key_type, c_key_type, result_type_name).unwrap();
        
        writeln!(&mut def, "\t\tDictionary_set(&d, &key_res.data.ok, &val_res.data.ok);").unwrap();
        
        // Cleanup the temporary key and value now that they've been copied into the dictionary
        let key_cleanup = self.get_cleanup_call(key_type, "key_res.data.ok", false);
        if !key_cleanup.is_empty() {
             writeln!(&mut def, "\t\t{};", key_cleanup).unwrap();
        }
        let val_cleanup = self.get_cleanup_call(value_type, "val_res.data.ok", false);
        if !val_cleanup.is_empty() {
             writeln!(&mut def, "\t\t{};", val_cleanup).unwrap();
        }

        writeln!(&mut def, "\t}}").unwrap();

        writeln!(&mut def, "\treturn ({}){{ .is_ok = true, .data.ok = d }};", result_type_name).unwrap();
        writeln!(&mut def, "}}").unwrap();
        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `list` of lists to a Jophet `Vector<Vector<T>>`.
    ///
    /// This function generates a C helper that:
    /// 1. Checks if the Python object is a `list`.
    /// 2. Creates the outer `JophetVector` (which will hold other `JophetVector`s).
    /// 3. Iterates over the outer list. For each inner item, it recursively calls the
    ///    appropriate `jophet_py_convert_to_vector_*` function.
    /// 4. If an inner list fails to convert, it **deep-deletes** the outer vector, cleaning up all
    ///    previously created inner vectors to prevent memory leaks, before propagating the error.
    /// 5. If all conversions succeed, it returns the final `JophetVector<JophetVector>`.
    pub fn get_or_create_py_to_vector_vector_helper(&mut self, inner_member_type: &JophetType) -> String {
        let mangled_inner_type = self.jophet_type_to_c_string_for_mangling(inner_member_type);

        let helper_name = format!("__jophet_py_to_vector_vector_{}", mangled_inner_type);
        // The return type is always Result_JophetVector_FfiError
        let result_type_name = "Result_JophetVector_FfiError".to_string();

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }
        self.function_prototypes.insert(proto);

        // Get details for the inner vector conversion
        let inner_vector_type = JophetType::Vector(Box::new(inner_member_type.clone()));
        let inner_conversion_fn = format!("jophet_py_convert_to_vector_{}", mangled_inner_type);
        
        // This helper will be used to clean up the outer vector if a conversion fails.
        // The outer vector contains `JophetVector`s, which might themselves contain owned data.
        let deep_delete_helper = self.get_or_create_vector_deep_delete_helper(&inner_vector_type);

        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyList_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python list.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        // The outer vector holds elements of type `JophetVector`.
        writeln!(&mut def, "\tJophetVector outer_vec = Vector_new(sizeof(JophetVector));").unwrap();
        writeln!(&mut def, "\tPy_ssize_t size = PyList_Size(handle);").unwrap();
        
        writeln!(&mut def, "\tfor (Py_ssize_t i = 0; i < size; i++) {{").unwrap();
        writeln!(&mut def, "\t\tPyObject* item = PyList_GetItem(handle, i);").unwrap();
        
        // Recursively call the helper for the inner vector
        writeln!(&mut def, "\t\t{} inner_res = {}(item);", result_type_name, inner_conversion_fn).unwrap();
        
        writeln!(&mut def, "\t\tif (!inner_res.is_ok) {{").unwrap();
        // **CRITICAL**: If an inner list fails to convert, we must deep-delete the outer vector
        // which contains all the successfully converted inner vectors up to this point.
        writeln!(&mut def, "\t\t\t{}(&outer_vec);", deep_delete_helper).unwrap();
        writeln!(&mut def, "\t\t\treturn ({}){{ .is_ok = false, .data.err = inner_res.data.err }};", result_type_name).unwrap();
        writeln!(&mut def, "\t\t}}").unwrap();
        
        // Push the successfully converted inner vector into the outer one.
        writeln!(&mut def, "\t\tVector_push(&outer_vec, &inner_res.data.ok);").unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        writeln!(&mut def, "\treturn ({}){{ .is_ok = true, .data.ok = outer_vec }};", result_type_name).unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C helper function to convert a Python `list` to a specific Jophet `Array`.
    ///
    /// This function generates a C helper that:
    /// 1. Checks if the Python object is a `list`.
    /// 2. Checks if the list's size matches the target array's size.
    /// 3. Iterates over the list, recursively calling the appropriate `jophet_py_convert_to_*`
    ///    function for each element.
    /// 4. If an element conversion fails, it correctly cleans up any previously converted
    ///    elements that are owned types to prevent memory leaks.
    /// 5. If all conversions succeed, it copies the temporary C array into the `Result` struct
    ///    and returns it.
    pub fn get_or_create_py_to_array_helper(
        &mut self,
        member_type: &JophetType,
        size: usize,
    ) -> String {
        let array_type = JophetType::Array {
            member_type: Box::new(member_type.clone()),
            size,
        };
        // The C type will be something like `int64_t[5]`, which is not a valid identifier.
        // We need a mangled name for the C function and Result struct.
        let mangled_member_type = self.jophet_type_to_c_string_for_mangling(member_type);
        let mangled_array_name = format!("Array_{}_{}", mangled_member_type, size);

        let helper_name = format!("__jophet_py_to_array_{}", mangled_array_name);
        let result_type_name = format!("Result_{}_FfiError", mangled_array_name);

        let proto = format!("{} {}(PythonObject handle);", result_type_name, helper_name);
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        self.function_prototypes.insert(proto);

        // Get details needed for the C code generation
        let c_member_type = self.jophet_type_to_c_string(member_type);
        let conversion_fn = format!("jophet_py_convert_to_{}", mangled_member_type);
        let member_result_type = self.jophet_type_to_c_string(&JophetType::Fallible {
            ok: Box::new(member_type.clone()),
            err: Box::new(JophetType::Error {
                name: "FfiError".to_string(),
                module_path: PathBuf::from("std"),
            }),
        });
        
        // This is critical: if an element is an owned type, we must clean up
        // partially converted elements if a later conversion fails.
        let member_cleanup_call = self.get_cleanup_call(member_type, "arr[j]", false);

        let mut def = String::new();
        writeln!(&mut def, "{} {}(PythonObject handle) {{", result_type_name, helper_name).unwrap();
        writeln!(&mut def, "\tif (!PyList_Check(handle)) {{").unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(\"Object is not a Python list.\");").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(&mut def, "\tif (PyList_Size(handle) != {}) {{", size).unwrap();
        writeln!(&mut def, "\t\tchar err_buf[128];").unwrap();
        writeln!(&mut def, "\t\tsnprintf(err_buf, sizeof(err_buf), \"Expected a list of size {}, but got size %zd.\", (size_t)PyList_Size(handle));", size).unwrap();
        writeln!(&mut def, "\t\tJophetString msg = String_new_from(err_buf);").unwrap();
        writeln!(&mut def, "\t\treturn ({}){{ .is_ok = false, .data.err = {{ .tag = FfiError_ConversionFailed, .data.Message = msg }} }};", result_type_name).unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        
        writeln!(&mut def, "\t{} arr[{}];", c_member_type, size).unwrap();
        writeln!(&mut def, "\t{} res;", result_type_name).unwrap();
        
        writeln!(&mut def, "\tfor (size_t i = 0; i < {}; ++i) {{", size).unwrap();
        writeln!(&mut def, "\t\tPyObject* item = PyList_GetItem(handle, i);").unwrap();
        writeln!(&mut def, "\t\t{} member_res = {}(item);", member_result_type, conversion_fn).unwrap();
        writeln!(&mut def, "\t\tif (!member_res.is_ok) {{").unwrap();
        // **CRITICAL**: If an element conversion fails, clean up previously converted owned elements.
        if !member_cleanup_call.is_empty() {
            writeln!(&mut def, "\t\t\tfor (size_t j = 0; j < i; ++j) {{").unwrap();
            writeln!(&mut def, "\t\t\t\t{};", member_cleanup_call).unwrap();
            writeln!(&mut def, "\t\t\t}}").unwrap();
        }
        writeln!(&mut def, "\t\t\treturn ({}){{ .is_ok = false, .data.err = member_res.data.err }};", result_type_name).unwrap();
        writeln!(&mut def, "\t\t}}").unwrap();
        writeln!(&mut def, "\t\tarr[i] = member_res.data.ok;").unwrap();
        writeln!(&mut def, "\t}}").unwrap();

        // C arrays can't be assigned directly, but they can be returned inside structs.
        // We must memcpy the stack-allocated array into the result struct's payload.
        writeln!(&mut def, "\tres.is_ok = true;").unwrap();
        writeln!(&mut def, "\tmemcpy(&res.data.ok, arr, sizeof(arr));").unwrap();
        writeln!(&mut def, "\treturn res;").unwrap();
        writeln!(&mut def, "}}").unwrap();
        writeln!(&mut self.function_defs, "{}", def).unwrap();
        helper_name
    }

    /// Gets or creates a C thunk function for comparing items of a specific type.
    /// This is used by the generic `minimum` and `maximum` collection helpers.
    pub fn get_or_create_comparison_thunk(&mut self, ty: &JophetType, is_max: bool) -> String {
        let mangled_type = self.jophet_type_to_c_string_for_mangling(ty);
        let thunk_name = format!(
            "__jophet_compare_thunk_{}_{}",
            mangled_type,
            if is_max { "max" } else { "min" }
        );

        let proto = format!("static bool {}(const void* a, const void* b);", thunk_name);
        if self.function_prototypes.contains(&proto) {
            return thunk_name;
        }
        self.function_prototypes.insert(proto);

        let c_type = self.jophet_type_to_c_string(ty);
        let comparison_op = if is_max { ">" } else { "<" };

        let mut def = String::new();
        writeln!(
            &mut def,
            "static bool {}(const void* a, const void* b) {{",
            thunk_name
        )
        .unwrap();
        writeln!(
            &mut def,
            "\treturn *(const {}*)a {} *(const {}*)b;",
            c_type, comparison_op, c_type
        )
        .unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}\n", def).unwrap();
        thunk_name
    }

    /// Gets or creates a C helper function to call Python's `min()` or `max()` on a Jophet `Vector<PythonObject>`.
    /// This replaces the non-portable GNU C statement expression with a standard C function.
    pub fn get_or_create_py_minmax_vector_helper(&mut self, python_builtin_name: &str) -> String {
        let helper_name = format!("__jophet_py_{}_vector", python_builtin_name);
        let proto = format!(
            "static PythonObject {}(const JophetVector* vec, const char* file, int line);",
            helper_name
        );
        if self.function_prototypes.contains(&proto) {
            return helper_name;
        }

        self.function_prototypes.insert(proto);

        let mut def = String::new();
        writeln!(
            &mut def,
            "static PythonObject {}(const JophetVector* vec, const char* file, int line) {{",
            helper_name
        )
        .unwrap();
        writeln!(&mut def, "\tPythonObject py_list = PyList_New(vec->len);").unwrap();
        writeln!(&mut def, "\tfor (size_t i = 0; i < vec->len; ++i) {{").unwrap();
        writeln!(
            &mut def,
            "\t\tPyObject* item = ((PythonObject*)vec->data)[i];"
        )
        .unwrap();
        writeln!(&mut def, "\t\tPy_INCREF(item);").unwrap();
        writeln!(&mut def, "\t\tPyList_SetItem(py_list, i, item);").unwrap();
        writeln!(&mut def, "\t}}").unwrap();
        writeln!(
            &mut def,
            "\tPythonObject result = jophet_py_call_builtin_or_panic(\"{}\", py_list, file, line);",
            python_builtin_name
        )
        .unwrap();
        writeln!(&mut def, "\tjophet_py_decref(py_list);").unwrap();
        writeln!(&mut def, "\treturn result;").unwrap();
        writeln!(&mut def, "}}").unwrap();

        writeln!(&mut self.function_defs, "{}\n", def).unwrap();
        helper_name
    }
}