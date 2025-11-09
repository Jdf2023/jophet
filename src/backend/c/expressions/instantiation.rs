// src/backend/c/expressions/instantiation.rs
//! Handles compilation of expressions that create new values/instances.

use super::super::Generator;
use super::CExpression;
use crate::core::ast::typed::*;
use std::fmt::Write;

impl Generator {
    /// Compiles a `new` expression for heap allocation.
    pub(super) fn compile_new_expression(
        &mut self,
        jophet_type: &JophetType,
        args: &[TypedExpression],
    ) -> CExpression {
        let compiled_args: Vec<String> =
            args.iter().map(|a| self.compile_expression(a)).collect();
        match jophet_type {
            JophetType::String => {
                self.runtime_needed = true;
                let result = if args.is_empty() {
                    "String_new()".to_string()
                } else {
                    format!("String_new_from({})", compiled_args[0])
                };
                CExpression::Simple(result)
            }
            JophetType::Vector(member_type) => {
                self.runtime_needed = true;
                let c_member_type = self.jophet_type_to_c_string(member_type);
                let result = if args.is_empty() {
                    format!("Vector_new(sizeof({}))", c_member_type)
                } else {
                    let array_size = if let JophetType::Array { size, .. } = &args[0].jophet_type {
                        *size
                    } else {
                        unreachable!("Semantic analysis ensures vector is initialized from an array")
                    };
                    format!(
                        "Vector_new_from_array(sizeof({}), {}, {})",
                        c_member_type, compiled_args[0], array_size
                    )
                };
                CExpression::Simple(result)
            }
            JophetType::Dictionary { key, value } => {
                self.compile_dictionary_instantiation_expression(key, value, &[])
            }
            JophetType::Struct { name, .. } => {
                let result = format!("{}_new({})", name, compiled_args.join(", "));
                CExpression::Simple(result)
            }
            _ => CExpression::Simple("".to_string()), // Should be caught by semantic analysis.
        }
    }

    /// Compiles an interpolated string expression.
    ///
    /// This is translated into a sequence of C statements that:
    /// 1. Create a new `JophetString` builder.
    /// 2. Append each literal and expression part to the builder using runtime helper functions.
    ///    For complex types, it generates and calls special `_sprint` helper functions.
    /// The function then returns the name of the temporary C variable holding the final string
    /// as a `CExpression::Temporary`.
    ///
    /// # Panics
    /// Panics if writing to the internal output buffer fails.
    pub(super) fn compile_interpolated_string_expression(
        &mut self,
        parts: &[TypedInterpolationPart],
    ) -> CExpression {
        self.runtime_needed = true;
        let builder_var = format!("__jophet_builder_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        writeln!(
            &mut self.output,
            "\tJophetString {} = String_new();",
            builder_var
        )
        .expect("Failed to write to internal buffer");

        for part in parts {
            match part {
                TypedInterpolationPart::Literal(s) => {
                    writeln!(
                        &mut self.output,
                        "\tString_builder_append(&{}, \"{}\");",
                        builder_var, s
                    )
                    .expect("Failed to write to internal buffer");
                }
                TypedInterpolationPart::Expression(expr) => {
                    let compiled_expr = self.compile_expression(expr);
                    let (append_func, arg) = match &expr.jophet_type {
                        JophetType::String => {
                            ("String_builder_append_string", format!("&{}", compiled_expr))
                        }
                        JophetType::StringSlice => ("String_builder_append", compiled_expr),
                        JophetType::Int(_) => ("String_builder_append_int64", compiled_expr),
                        JophetType::Float(_) => ("String_builder_append_float64", compiled_expr),
                        JophetType::Char => ("String_builder_append_char", compiled_expr),
                        JophetType::Bool => ("String_builder_append_bool", compiled_expr),
                        // Handle aggregate types by calling a generic sprint helper
                        ty if self.is_struct_like(ty) => {
                            self.runtime_needed = true;
                            // The data argument needs to be a pointer. Use ensure_lvalue to handle temporaries.
                            let lvalue = self.ensure_lvalue(expr);
                            let data_arg = format!("&{}", lvalue);

                            let (sprint_fn_name, sprint_fn_def) = self.get_or_create_sprint_fn(ty);
                            if let Some(def) = sprint_fn_def {
                                // Use function_defs to store these helpers to avoid redefinition.
                                if self.sprint_helpers.insert(sprint_fn_name.clone()) {
                                    writeln!(&mut self.function_defs, "{}\n", def).unwrap();
                                }
                            }
                            (
                                "jophet_sprint",
                                format!(
                                    "{}, (void (*)(JophetString*, const void*))&{}",
                                    data_arg, sprint_fn_name
                                ),
                            )
                        }
                        _ => ("/* unsupported format type */", "".to_string()),
                    };

                    // The generic sprint function needs the builder passed by pointer.
                    if append_func == "jophet_sprint" {
                        writeln!(
                            &mut self.output,
                            "\t{}(&{}, {});",
                            append_func, builder_var, arg
                        )
                        .expect("Failed to write to internal buffer");
                    } else {
                        writeln!(
                            &mut self.output,
                            "\t{}(&{}, {});",
                            append_func, builder_var, arg
                        )
                        .expect("Failed to write to internal buffer");
                    }
                }
            }
        }
        // The expression's value is the name of the variable holding the built string.
        CExpression::Temporary(builder_var)
    }

    /// Compiles a struct instantiation expression to a C compound literal.
    /// Example: `MyStruct(1, 2)` -> `(MyStruct){ 1, 2 }`
    pub(super) fn compile_struct_instantiation_expression(
        &mut self,
        name: &str,
        args: &[(String, TypedExpression)],
    ) -> CExpression {
        let compiled_args: Vec<String> =
            args.iter().map(|(_, a)| self.compile_expression(a)).collect();
        let result = format!("({}){{ {} }}", name, compiled_args.join(", "));
        CExpression::Simple(result)
    }

    /// Compiles a union instantiation to a C compound literal with a designated initializer.
    /// Example: `MyUnion(f: 3.14)` -> `(MyUnion){ .f = 3.14 }`
    pub(super) fn compile_union_instantiation_expression(
        &mut self,
        union_name: &str,
        field_name: &str,
        value: &TypedExpression,
    ) -> CExpression {
        let compiled_value = self.compile_expression(value);
        let sanitized_field_name = self.sanitize_c_keyword(field_name);
        let result = format!(
            "({}){{ .{} = {} }}",
            union_name, sanitized_field_name, compiled_value
        );
        CExpression::Simple(result)
    }

    /// Compiles a tagged union instantiation to a C compound literal with designated initializers.
    /// This sets the `tag` and the correct field within the `data` union. It now correctly
    /// handles payload-less variants.
    /// Example: `MyEnum.Variant(42)` -> `(MyEnum){ .tag = MyEnum_Variant, .data.Variant = 42 }`
    /// Example: `MyEnum.Quit` -> `(MyEnum){ .tag = MyEnum_Quit }`
    pub(super) fn compile_tagged_union_instantiation(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        payload: &Option<Box<TypedExpression>>,
    ) -> CExpression {
        let tag = format!("{}_{}", enum_name, variant_name);
        let result = if let Some(p) = payload {
            let payload_str = self.compile_expression(p);
            format!(
                "({}){{ .tag = {}, .data.{} = {} }}",
                enum_name, tag, variant_name, payload_str
            )
        } else {
            format!("({}){{ .tag = {} }}", enum_name, tag)
        };
        CExpression::Simple(result)
    }

    /// Compiles a tuple expression to a C compound literal for the corresponding tuple struct.
    /// Example: `(1, "a")` -> `(Tuple_Int64_String){ 1, "a" }`
    pub(super) fn compile_tuple_expression(
        &mut self,
        elements: &[TypedExpression],
        tuple_type: &JophetType,
    ) -> CExpression {
        let c_type_name = self.jophet_type_to_c_string(tuple_type);
        let result = format!(
            "({}){{ {} }}",
            c_type_name,
            elements
                .iter()
                .map(|e| self.compile_expression(e))
                .collect::<Vec<_>>()
                .join(", ")
        );
        CExpression::Simple(result)
    }

    /// Compiles an array literal expression to a C array initializer list.
    /// Example: `[1, 2, 3]` -> `{ 1, 2, 3 }`
    pub(super) fn compile_array_literal_expression(
        &mut self,
        elements: &[TypedExpression],
    ) -> CExpression {
        let compiled_elements = elements
            .iter()
            .map(|e| self.compile_expression(e))
            .collect::<Vec<_>>()
            .join(", ");
        let result = format!("{{ {} }}", compiled_elements);
        CExpression::Simple(result)
    }

    /// Compiles a dictionary instantiation into C statements that create and populate a dictionary.
    /// It now correctly generates and passes function pointers for deep-cloning and deep-deleting
    /// the dictionary's keys and values if they are owned types. This prevents memory leaks and
    /// double-frees.
    pub(super) fn compile_dictionary_instantiation_expression(
        &mut self,
        key_type: &JophetType,
        value_type: &JophetType,
        pairs: &[(TypedExpression, TypedExpression)],
    ) -> CExpression {
        self.runtime_needed = true;

        let dict_var = format!("__jophet_dict_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        // Generate the C code to create a new dictionary.
        let c_key_type = self.jophet_type_to_c_string(key_type);
        let c_value_type = self.jophet_type_to_c_string(value_type);

        // Get the appropriate delete/clone function pointers for the key and value types.
        let key_del_fn = if self.type_needs_cleanup(key_type) {
            format!("&{}", self.get_or_create_item_delete_thunk(key_type))
        } else {
            "NULL".to_string()
        };

        let val_del_fn = if self.type_needs_cleanup(value_type) {
            format!("&{}", self.get_or_create_item_delete_thunk(value_type))
        } else {
            "NULL".to_string()
        };

        let key_clone_fn = if self.type_is_cloneable(key_type) {
            format!("&{}", self.get_or_create_item_clone_thunk(key_type))
        } else {
            "NULL".to_string()
        };

        let val_clone_fn = if self.type_is_cloneable(value_type) {
            format!("&{}", self.get_or_create_item_clone_thunk(value_type))
        } else {
            "NULL".to_string()
        };

        writeln!(&mut self.output, "\tJophetDictionary {} = Dictionary_new(sizeof({}), sizeof({}), {}, {}, {}, {});",
            dict_var, c_key_type, c_value_type, key_del_fn, val_del_fn, key_clone_fn, val_clone_fn).unwrap();

        // Generate C code to insert each key-value pair.
        for (key_expr, value_expr) in pairs {
            // Use `ensure_lvalue` to handle temporary variables cleanly.
            let key_lvalue = self.ensure_lvalue(key_expr);
            let value_lvalue = self.ensure_lvalue(value_expr);
            
            writeln!(
                &mut self.output,
                "\tDictionary_set(&{}, &{}, &{});",
                dict_var, key_lvalue, value_lvalue
            )
            .unwrap();
        }

        // The expression's value is the name of the variable holding the dictionary.
        CExpression::Temporary(dict_var)
    }

    /// Compiles a closure expression into a C `JophetClosure` struct literal.
    ///
    /// This function performs several steps:
    /// 1. It generates a `typedef struct` for the closure's environment. This is always
    ///    generated; for closures with no captures, it contains a dummy member to be valid C.
    /// 2. It generates a static C "destructor" function for the environment, which
    ///    handles the cleanup of any owned types within it (e.g., Strings). This is only
    ///    generated if there are captures.
    /// 3. It generates a static C "cloner" function for the environment, which performs
    ///    a deep copy of the environment and all its owned data. This is only generated
    ///    if there are captures.
    /// 4. It generates C statements to `malloc` an instance of this environment struct and
    ///    populate it by **cloning** the current values of the captured variables. This
    ///    prevents double-free errors. These statements are emitted into the current output buffer.
    /// 5. It returns a C compound literal `(JophetClosure){...}` which initializes the
    ///    generic closure struct with the function pointer, the new environment, and pointers
    ///    to the environment's destructor and cloner functions.
    pub(super) fn compile_closure_expression(
        &mut self,
        function: &TypedFunctionDecl,
        captures: &[TypedCapturedVariable],
    ) -> CExpression {
        self.runtime_needed = true; // For malloc and closure helpers
        let env_struct_name = format!("{}_env", function.mangled_name);

        // 1. Generate the environment struct definition. It's always needed for the function signature.
        let env_struct_def = if !captures.is_empty() {
            let mut env_fields = Vec::new();
            for cap in captures {
                let c_type = self.jophet_type_to_c_string(&cap.jophet_type);
                env_fields
                    .push(format!("{} {};", c_type, self.sanitize_c_keyword(&cap.name)));
            }
            format!(
                "typedef struct {{ {} }} {};",
                env_fields.join(" "),
                env_struct_name
            )
        } else {
            // C doesn't allow empty structs, so add a dummy member for zero-capture closures.
            format!("typedef struct {{ uint8_t _dummy; }} {};", env_struct_name)
        };
        self.type_defs.insert(env_struct_def);

        // 2. Generate the environment destructor and cloner functions.
        let delete_env_fn_name = format!("{}_delete_env", function.mangled_name);
        let clone_env_fn_name = format!("{}_clone_env", function.mangled_name);
        let mut clone_fn_ptr = "NULL".to_string();

        if !captures.is_empty() {
            // Destructor
            let mut destructor_body = String::new();
            writeln!(
                &mut destructor_body,
                "\t{}* env = ({}*)env_ptr;",
                env_struct_name, env_struct_name
            )
            .unwrap();
            for cap in captures {
                // For each captured variable, get its cleanup call (e.g., `String_delete(&env->my_str);`)
                let cleanup_call = self.get_cleanup_call(
                    &cap.jophet_type,
                    &format!("env->{}", self.sanitize_c_keyword(&cap.name)),
                    false,
                );
                if !cleanup_call.is_empty() {
                    writeln!(&mut destructor_body, "\t{};", cleanup_call).unwrap();
                }
            }
            writeln!(&mut destructor_body, "\tfree(env);").unwrap();

            let destructor_def = format!(
                "static void {}(void* env_ptr) {{\n{}}}",
                delete_env_fn_name, destructor_body
            );
            writeln!(&mut self.function_defs, "{}\n", destructor_def).unwrap();

            // Cloner
            let mut cloner_body = String::new();
            writeln!(
                &mut cloner_body,
                "\tconst {}* old_env = (const {}*)env_ptr;",
                env_struct_name, env_struct_name
            )
            .unwrap();
            writeln!(
                &mut cloner_body,
                "\t{}* new_env = ({}*)malloc(sizeof({}));",
                env_struct_name, env_struct_name, env_struct_name
            )
            .unwrap();

            for cap in captures {
                let sanitized_cap_name = self.sanitize_c_keyword(&cap.name);
                let clone_call =
                    self.get_clone_call(&cap.jophet_type, &format!("old_env->{}", sanitized_cap_name));
                if !clone_call.is_empty() {
                    writeln!(
                        &mut cloner_body,
                        "\tnew_env->{} = {};",
                        sanitized_cap_name, clone_call
                    )
                    .unwrap();
                }
            }
            writeln!(&mut cloner_body, "\treturn new_env;").unwrap();

            let cloner_def = format!(
                "static void* {}(const void* env_ptr) {{\n{}}}",
                clone_env_fn_name, cloner_body
            );
            writeln!(&mut self.function_defs, "{}\n", cloner_def).unwrap();
            clone_fn_ptr = format!("&{}", clone_env_fn_name);
        }

        // 3. Generate code to create and populate the environment instance.
        let env_var_name = format!("__env_{}", self.temp_var_counter);
        self.temp_var_counter += 1;

        if !captures.is_empty() {
            writeln!(&mut self.output, "\t{}* {} = ({}*)malloc(sizeof({}));", env_struct_name, env_var_name, env_struct_name, env_struct_name).unwrap();
            for cap in captures {
                // Use a deep clone for captured variables to prevent double-frees.
                let sanitized_cap_name = self.sanitize_c_keyword(&cap.name);
                let clone_call = self.get_clone_call(&cap.jophet_type, &sanitized_cap_name);
                writeln!(
                    &mut self.output,
                    "\t{}->{} = {};",
                    env_var_name, sanitized_cap_name, clone_call
                )
                .unwrap();
            }
        } else {
            // If there are no captures, the environment pointer is NULL.
            writeln!(&mut self.output, "\tvoid* {} = NULL;", env_var_name).unwrap();
        }

        // 4. Return the compound literal for the JophetClosure struct, including the destructor.
        let delete_fn_ptr = if !captures.is_empty() {
            format!("&{}", delete_env_fn_name)
        } else {
            "NULL".to_string()
        };

        let result = format!(
            "(JophetClosure){{ .fn_ptr = (void (*)(void)){}, .env = {}, .delete_env_fn = {}, .clone_env_fn = {} }}",
            function.mangled_name,
            env_var_name,
            delete_fn_ptr,
            clone_fn_ptr
        );
        CExpression::Simple(result)
    }

    /// Compiles a `parse(Type, String)` expression into a call to a C runtime helper.
    /// It now uses the `ensure_lvalue` helper to correctly handle temporary r-value string arguments.
    pub(super) fn compile_parse_expression(
        &mut self,
        target_type: &JophetType,
        parse_expr: &TypedExpression,
    ) -> CExpression {
        self.runtime_needed = true;
        // Use ensure_lvalue to create a temporary for r-value strings and get a stable address.
        let lvalue = self.ensure_lvalue(parse_expr);
        let c_str_arg = format!("&{}", lvalue);

        let runtime_fn_name = match target_type {
            JophetType::Int(8) => "parse_int8",
            JophetType::Int(16) => "parse_int16",
            JophetType::Int(32) => "parse_int32",
            JophetType::Int(64) => "parse_int64",
            JophetType::UInt(8) => "parse_uint8",
            JophetType::UInt(16) => "parse_uint16",
            JophetType::UInt(32) => "parse_uint32",
            JophetType::UInt(64) => "parse_uint64",
            JophetType::Float(32) => "parse_float32",
            JophetType::Float(64) => "parse_float64",
            _ => unreachable!("Semantic analysis should prevent parsing non-numeric types"),
        };

        let result = format!("{}({})", runtime_fn_name, c_str_arg);
        CExpression::Simple(result)
    }

    /// Compiles the implicit wrapping of a value into a fallible (`Result`) type.
    /// This is used by the semantic analyzer to automatically convert `T` or `E` into `Result<T, E>`.
    ///
    /// It generates a C compound literal for the `Result` struct, setting the `is_ok` flag
    /// and initializing the appropriate field in the `data` union. It now correctly handles
    /// the upcasting of specific errors into the universal `JophetError` type.
    pub(super) fn compile_fallible_wrap_expression(
        &mut self,
        result_type: &JophetType,
        is_ok: bool,
        expr: &TypedExpression,
    ) -> CExpression {
        let result_c_type = self.jophet_type_to_c_string(result_type);
        let value = self.compile_expression(expr);
        let result = if is_ok {
            format!(
                "({}){{ .is_ok = true, .data.ok = {} }}",
                result_c_type, value
            )
        } else {
            format!(
                "({}){{ .is_ok = false, .data.err = {} }}",
                result_c_type, value
            )
        };
        CExpression::Simple(result)
    }

    /// Compiles an `ErrorUpcast` expression.
    /// This generates the C code to construct a universal `JophetError` from a specific error type.
    pub(super) fn compile_error_upcast_expression(
        &mut self,
        inner_expr: &TypedExpression,
    ) -> CExpression {
        let specific_error_type_name = if let JophetType::Error { name, .. } = &inner_expr.jophet_type {
            name
        } else {
            unreachable!("ErrorUpcast must contain a JophetType::Error")
        };
        let compiled_inner = self.compile_expression(inner_expr);
        let result = format!(
            "(JophetError){{ .tag = JophetError_{}, .data.{} = {} }}",
            specific_error_type_name, specific_error_type_name, compiled_inner
        );
        CExpression::Simple(result)
    }
}