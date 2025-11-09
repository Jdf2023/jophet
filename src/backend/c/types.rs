// src/backend/c/types.rs
//! Handles the mapping of Jophet types to their C language string representations.
//!
//! This module is crucial for the C backend as it provides the functions to translate
//! `JophetType` enum variants from the typed AST into the corresponding C type names
//! (e.g., `JophetType::Int(32)` becomes `"int32_t"`). It also handles the on-the-fly
//! generation of C definitions for complex types like tuples, fallible results, and the
//! universal `JophetError` type. It now handles the `Closure` and generic `PythonObject`
//! types, translating them into their C struct representations. It also generates
//! struct wrappers for array types when they are used as function return values.

use super::Generator;
use crate::core::ast::typed::JophetType;
use std::fmt::Write;

impl Generator {
    /// Recursively builds the dimension suffix string for a C array declaration.
    ///
    /// For a type like `Array<Array<Int64, 3>, 2>`, this function will produce `"[2][3]"`.
    ///
    /// # Arguments
    /// * `t` - The JophetType to inspect.
    ///
    /// # Returns
    /// A string containing all array dimension specifiers.
    pub fn get_array_dimension_suffix(&mut self, t: &JophetType) -> String {
        let mut suffix = String::new();
        let mut current_type = t;
        while let JophetType::Array { member_type, size } = current_type {
            suffix.push_str(&format!("[{}]", size));
            current_type = member_type;
        }
        suffix
    }

    /// Converts a `JophetType` into its corresponding C type string for a function return value.
    /// For array types, this function generates and returns the name of a C struct wrapper.
    /// For all other types, it delegates to `jophet_type_to_c_string`.
    pub(super) fn jophet_type_to_c_return_string(&mut self, t: &JophetType) -> String {
        if let JophetType::Array { .. } = t {
            // This is an array being returned. We must wrap it in a struct.
            let base_type_str = self.jophet_type_to_c_string(&self.get_array_base_type(t));
            let dimensions_str = self.get_array_dimension_suffix(t);
            let mangled_name = format!(
                "__JophetArrayReturn_{}_{}",
                self.jophet_type_to_c_string_for_mangling(&self.get_array_base_type(t)),
                self.get_array_total_size(t)
            );

            // Generate the struct definition if it doesn't exist yet.
            if !self.array_return_structs.contains(&mangled_name) {
                let struct_def = format!(
                    "typedef struct {{ {} data{}; }} {};",
                    base_type_str, dimensions_str, mangled_name
                );
                self.type_defs.insert(struct_def);
                self.array_return_structs.insert(mangled_name.clone());
            }
            return mangled_name;
        }
        // For all non-array types, the return type is the same as the normal type.
        self.jophet_type_to_c_string(t)
    }

    /// Converts a `JophetType` into its corresponding C base type name as a string for use in
    /// variables, parameters, and fields.
    ///
    /// For array types, this function recursively unwraps them to find the innermost
    /// non-array type (e.g., for `Array<Array<Int64, 3>, 2>` it returns "int64_t"). The
    /// dimension suffix is added separately. For other types, it functions as a direct
    /// translator. It also sets the `runtime_needed` flag if any resolved type is defined
    /// in the C runtime.
    pub fn jophet_type_to_c_string(&mut self, t: &JophetType) -> String {
        match t {
            // FIX [E0004]: Handle the ErrorSentinel case.
            JophetType::ErrorSentinel => "/* <type error> */ void".to_string(),
            // Primitive types are mapped to C99 fixed-width integer types.
            JophetType::Int(8) => "int8_t".to_string(),
            JophetType::Int(16) => "int16_t".to_string(),
            JophetType::Int(32) => "int32_t".to_string(),
            JophetType::Int(64) => "int64_t".to_string(),
            JophetType::UInt(8) => "uint8_t".to_string(),
            JophetType::UInt(16) => "uint16_t".to_string(),
            JophetType::UInt(32) => "uint32_t".to_string(),
            JophetType::UInt(64) => "uint64_t".to_string(),
            JophetType::Float(32) => "float".to_string(),
            JophetType::Float(64) => "double".to_string(),
            JophetType::Bool => "bool".to_string(),
            JophetType::Char => "char".to_string(),
            JophetType::USize => "size_t".to_string(),
            JophetType::ISize => "ptrdiff_t".to_string(),

            // Built-in Jophet types are mapped to their C runtime struct names.
            JophetType::String => {
                self.runtime_needed = true;
                "JophetString".to_string()
            },
            JophetType::StringSlice => "const char*".to_string(),
            JophetType::Vector(_) => {
                self.runtime_needed = true;
                "JophetVector".to_string()
            },
            JophetType::Dictionary { .. } => {
                self.runtime_needed = true;
                "JophetDictionary".to_string()
            }
            JophetType::Nothing => "void".to_string(),

            // Module types don't have a direct C representation as a variable type.
            JophetType::Module { .. } => "/* module */".to_string(),
            
            // Traits and Generic Parameters are compile-time only constructs.
            // They do not have a direct C representation after monomorphization.
            JophetType::Trait { .. } => "/* trait */".to_string(),
            JophetType::GenericParam { .. } => "/* generic */".to_string(),

            // User-defined types use their original names, which are defined via `typedef`.
            JophetType::Struct { name, .. } => name.clone(),
            JophetType::Enum { name, .. } => name.clone(),
            JophetType::Union { name, .. } => name.clone(),
            JophetType::TaggedUnion { name, .. } => name.clone(),
            JophetType::Error { name, .. } => {
                // If this is one of the built-in error types, we need the runtime.
                if self.predefined_runtime_types.contains(name.as_str()) {
                    self.runtime_needed = true;
                }
                name.clone()
            },
            JophetType::AnyError => {
                self.runtime_needed = true;
                // Generate the universal JophetError struct on-demand, but only once.
                let struct_name = "JophetError".to_string();
                let def_marker = format!("typedef struct {} {{", struct_name);

                if self.type_defs.iter().all(|def| !def.starts_with(&def_marker)) {
                    // 1. Tag Enum
                    let tag_enum_name = format!("{}_Tag", struct_name);
                    let variant_names: Vec<String> = self.all_error_types.iter().map(|err_name| format!("{}_{}", struct_name, err_name)).collect();
                    let tag_enum_body = if variant_names.is_empty() {
                        // C requires at least one enum member.
                        "_JophetError_DummyTag".to_string()
                    } else {
                        variant_names.join(", ")
                    };
                    let tag_enum = format!("typedef enum {{ {} }} {};", tag_enum_body, tag_enum_name);
                    self.type_defs.insert(tag_enum);

                    // 2. Data Union
                    let data_union_name = format!("{}_Data", struct_name);
                    let union_fields: Vec<String> = self.all_error_types.iter().map(|err_name| format!("{} {};", err_name, err_name)).collect();
                    let union_body = if union_fields.is_empty() {
                        // C requires at least one union member.
                        "uint8_t _dummy_data;".to_string()
                    } else {
                        union_fields.join(" ")
                    };
                    let data_union = format!("typedef union {{ {} }} {};", union_body, data_union_name);
                    self.type_defs.insert(data_union);
                    
                    // 3. Final Struct
                    let final_struct = format!("typedef struct {} {{ {} tag; {} data; }} {};", struct_name, tag_enum_name, data_union_name, struct_name);
                    self.type_defs.insert(final_struct);
                    
                    // 4. Print Function
                    let mut print_body = String::new();
                    writeln!(&mut print_body, "\tswitch (s->tag) {{").unwrap();
                    for err_name in &self.all_error_types {
                        let full_variant_name = format!("{}_{}", struct_name, err_name);
                        writeln!(&mut print_body, "\t\tcase {}: {{", full_variant_name).unwrap();
                        writeln!(&mut print_body, "\t\t\t{}_print(&s->data.{});", err_name, err_name).unwrap();
                        writeln!(&mut print_body, "\t\t\tbreak;").unwrap();
                        writeln!(&mut print_body, "\t\t}}").unwrap();
                    }
                    if self.all_error_types.is_empty() {
                         writeln!(&mut print_body, "\t\tdefault: break;").unwrap();
                    }
                    writeln!(&mut print_body, "\t}}").unwrap();

                    let print_fn_name = format!("{}_print", struct_name);
                    let print_proto = format!("void {}(const {}* s);", print_fn_name, struct_name);
                    self.function_prototypes.insert(print_proto);

                    let print_def = format!("void {}(const {}* s) {{\n{}}}", print_fn_name, struct_name, print_body);
                    writeln!(&mut self.function_defs, "{}\n", print_def).unwrap();
                }

                struct_name
            }

            // Pointers and references are translated to C pointers.
            JophetType::Pointer(inner) => format!("{}*", self.jophet_type_to_c_string(inner)),
            JophetType::Reference(inner) => format!("{}*", self.jophet_type_to_c_string(inner)),
            JophetType::MutableReference(inner) => {
                format!("{}*", self.jophet_type_to_c_string(inner))
            }
            JophetType::RawPointer(inner) => format!("{}*", self.jophet_type_to_c_string(inner)),

            // For array types, recurse to find the base member type.
            JophetType::Array { member_type, .. } => self.jophet_type_to_c_string(member_type),

            // Tuples are dynamically translated into named C structs.
            JophetType::Tuple(types) => {
                let c_types: Vec<String> = types
                    .iter()
                    .map(|p| self.jophet_type_to_c_string(p))
                    .collect();
                // Generate a unique, deterministic name for the tuple struct.
                let struct_name =
                    format!("Tuple_{}", c_types.join("_").replace('*', "ptr").replace(' ', ""));

                // Generate the struct fields `f0`, `f1`, etc.
                let fields = c_types
                    .iter()
                    .enumerate()
                    .map(|(i, ct)| format!("{} f{};", ct, i))
                    .collect::<Vec<_>>()
                    .join(" ");

                // Add the new struct definition to the set of types to be emitted.
                self.type_defs
                    .insert(format!("typedef struct {{ {} }} {};", fields, struct_name));
                struct_name
            }

            // Fallible types (`Type?`) are dynamically translated into a `Result` struct.
            JophetType::Fallible { ok, err } => {
                // When resolving a fallible type, we must also resolve its inner types
                // to correctly propagate the `runtime_needed` flag.
                let ok_c_type_for_name = self.jophet_type_to_c_string(ok);
                let err_c_type_for_name = self.jophet_type_to_c_string(err);
                
                // The name used for the typedef is based on the original type names.
                let name = format!("Result_{}_{}", ok_c_type_for_name, err_c_type_for_name)
                    .replace('*', "ptr")
                    .replace(' ', "");

                // The struct contains a boolean flag and a union for the `ok` or `err` value.
                if !self.predefined_runtime_types.contains(&name) {
                    // For the struct definition itself, we must handle `void`. C structs cannot have `void` members.
                    // We replace them with a dummy `int` and use a special field name to avoid collisions.
                    let ok_c_type_for_def = if ok_c_type_for_name == "void" { "int".to_string() } else { ok_c_type_for_name.clone() };
                    let err_c_type_for_def = if err_c_type_for_name == "void" { "int".to_string() } else { err_c_type_for_name.clone() };

                    let ok_field = if ok_c_type_for_name == "void" { "_dummy_ok" } else { "ok" };
                    let err_field = if err_c_type_for_name == "void" { "_dummy_err" } else { "err" };
                    
                    let struct_def = format!(
                        "typedef struct {} {{ bool is_ok; union {{ {} {}; {} {}; }} data; }} {};",
                        name, ok_c_type_for_def, ok_field, err_c_type_for_def, err_field, name
                    );
                    self.type_defs.insert(struct_def);
                }
                name
            }

            // Function pointers are represented as void pointers for simplicity in this backend.
            JophetType::Function { .. } => "void*".to_string(),

            // Closures are represented by a generic struct that holds the function pointer
            // and a pointer to the captured environment.
            JophetType::Closure { .. } => {
                self.runtime_needed = true;
                "JophetClosure".to_string()
            }
            
            JophetType::CLibrary { .. } => "/* C Library Handle */ void*".to_string(),
            JophetType::PythonModule => {
                self.python_runtime_needed = true;
                "PythonModule".to_string()
            }
            JophetType::PythonObject { .. } | JophetType::PythonSlice => {
                self.python_runtime_needed = true;
                // All branded PythonObjects and PythonSlices map to the same C typedef.
                // The brand is erased during code generation.
                "PythonObject".to_string()
            }

            // This is an internal-only type for the analyzer and should not reach the backend.
            JophetType::UnsizedArray(_) => unreachable!("UnsizedArray type should not exist in the backend"),

            // These should not be encountered after semantic analysis has resolved concrete types.
            JophetType::Int(_) => "int_t_unknown".to_string(),
            JophetType::UInt(_) => "uint_t_unknown".to_string(),
            JophetType::Float(_) => "float_t_unknown".to_string(),
        }
    }

    /// Converts a `JophetType` into a C-safe string for use in mangled function names.
    /// This replaces characters that are invalid in C identifiers.
    pub fn jophet_type_to_c_string_for_mangling(&mut self, t: &JophetType) -> String {
        self.jophet_type_to_c_string(t)
            .replace('*', "ptr")
            .replace(' ', "_")
    }

    /// Converts a JophetType into a C enum tag for the FFI runtime.
    pub fn jophet_type_to_c_enum_tag(&self, t: &JophetType) -> String {
        match t {
            JophetType::Int(8) => "JOPHET_TYPE_INT8".to_string(),
            JophetType::Int(16) => "JOPHET_TYPE_INT16".to_string(),
            JophetType::Int(32) => "JOPHET_TYPE_INT32".to_string(),
            JophetType::Int(64) => "JOPHET_TYPE_INT64".to_string(),
            JophetType::UInt(8) => "JOPHET_TYPE_UINT8".to_string(),
            JophetType::UInt(16) => "JOPHET_TYPE_UINT16".to_string(),
            JophetType::UInt(32) => "JOPHET_TYPE_UINT32".to_string(),
            JophetType::UInt(64) => "JOPHET_TYPE_UINT64".to_string(),
            JophetType::Float(32) => "JOPHET_TYPE_FLOAT32".to_string(),
            JophetType::Float(64) => "JOPHET_TYPE_FLOAT64".to_string(),
            JophetType::Bool => "JOPHET_TYPE_BOOL".to_string(),
            JophetType::Char => "JOPHET_TYPE_CHAR".to_string(),
            JophetType::String => "JOPHET_TYPE_STRING".to_string(),
            JophetType::StringSlice => "JOPHET_TYPE_STRING_SLICE".to_string(),
            JophetType::Enum { .. } => "JOPHET_TYPE_ENUM".to_string(),
            JophetType::Vector(inner) => match inner.as_ref() {
                JophetType::Int(8) => "JOPHET_TYPE_VECTOR_I8".to_string(),
                JophetType::Int(16) => "JOPHET_TYPE_VECTOR_I16".to_string(),
                JophetType::Int(32) => "JOPHET_TYPE_VECTOR_I32".to_string(),
                JophetType::Int(64) => "JOPHET_TYPE_VECTOR_I64".to_string(),
                JophetType::UInt(8) => "JOPHET_TYPE_VECTOR_U8".to_string(),
                JophetType::UInt(16) => "JOPHET_TYPE_VECTOR_U16".to_string(),
                JophetType::UInt(32) => "JOPHET_TYPE_VECTOR_U32".to_string(),
                JophetType::UInt(64) => "JOPHET_TYPE_VECTOR_U64".to_string(),
                JophetType::Float(32) => "JOPHET_TYPE_VECTOR_F32".to_string(),
                JophetType::Float(64) => "JOPHET_TYPE_VECTOR_F64".to_string(),
                JophetType::String => "JOPHET_TYPE_VECTOR_STRING".to_string(),
                JophetType::Bool => "JOPHET_TYPE_VECTOR_BOOL".to_string(),
                JophetType::Char => "JOPHET_TYPE_VECTOR_CHAR".to_string(),
                JophetType::Vector(inner_inner) => match inner_inner.as_ref() {
                    JophetType::Int(64) => "JOPHET_TYPE_VECTOR_VECTOR_I64".to_string(),
                    _ => "JOPHET_TYPE_UNKNOWN".to_string(),
                },
                _ => "JOPHET_TYPE_UNKNOWN".to_string(),
            },
            JophetType::Array { member_type, .. } => match member_type.as_ref() {
                JophetType::Array { member_type: inner_member, .. } => match inner_member.as_ref() {
                    JophetType::Int(64) => "JOPHET_TYPE_VECTOR_VECTOR_I64".to_string(),
                    _ => "JOPHET_TYPE_UNKNOWN".to_string(),
                },
                // Any other 1D array will be converted to a Vector, so we get the vector's tag.
                _ => self.jophet_type_to_c_enum_tag(&JophetType::Vector(member_type.clone())),
            },
            JophetType::Tuple(_) => "JOPHET_TYPE_TUPLE".to_string(),
            JophetType::Struct { .. } => "JOPHET_TYPE_STRUCT".to_string(),
            JophetType::Dictionary { .. } => "JOPHET_TYPE_DICTIONARY".to_string(),
            // **THE FIX**: When a value is already a PythonObject, we use a specific tag
            // to tell the C runtime not to perform any conversion.
            JophetType::PythonObject { .. } | JophetType::PythonModule => {
                "JOPHET_TYPE_PYTHON_OBJECT".to_string()
            }
            JophetType::PythonSlice => "JOPHET_TYPE_PYTHON_SLICE".to_string(),
            JophetType::TaggedUnion { .. } => "JOPHET_TYPE_TAGGED_UNION".to_string(),
            JophetType::Error { .. } => "JOPHET_TYPE_ERROR".to_string(),
            _ => "JOPHET_TYPE_UNKNOWN".to_string(),
        }
    }

    /// Determines the correct C `printf` format specifier for a given `JophetType`.
    ///
    /// This is used when compiling the built-in `print` and `println` functions.
    /// It returns a C expression fragment (like `"%d"` or `"%zu"`) which will be
    /// part of a sequence of concatenated C string literals.
    pub fn get_format_specifier(&self, t: &JophetType) -> String {
        match t {
            // For PRI macros, return an expression that the C preprocessor will concatenate.
            JophetType::Int(8) => "\"%\" PRId8".to_string(),
            JophetType::Int(16) => "\"%\" PRId16".to_string(),
            JophetType::Int(32) => "\"%\" PRId32".to_string(),
            JophetType::Int(64) => {
                if cfg!(windows) {
                    "\"%lld\"".to_string()
                } else {
                    "\"%\" PRId64".to_string()
                }
            }
            JophetType::UInt(8) => "\"%\" PRIu8".to_string(),
            JophetType::UInt(16) => "\"%\" PRIu16".to_string(),
            JophetType::UInt(32) => "\"%\" PRIu32".to_string(),
            JophetType::UInt(64) => {
                // Use "%zu" for size_t, which is the most portable specifier.
                // This assumes UInt64 is primarily used for sizes/lengths.
                "\"%zu\"".to_string()
            }
            JophetType::USize => "\"%zu\"".to_string(),
            JophetType::ISize => "\"%td\"".to_string(),
            // For standard literals, wrap them in quotes to make them C string literals.
            JophetType::Float(_) => "\"%g\"".to_string(),
            // Bools are printed as "true" or "false" strings, so we use the string specifier.
            JophetType::Bool => "\"%s\"".to_string(),
            JophetType::Char => "\"%c\"".to_string(),
            JophetType::String => "\"%.*s\"".to_string(),
            JophetType::StringSlice => "\"%s\"".to_string(),
            JophetType::Pointer(_) => "\"%p\"".to_string(),
            JophetType::Reference(inner) if **inner == JophetType::String => "\"%.*s\"".to_string(),
            JophetType::Reference(_) => "\"%p\"".to_string(),
            JophetType::MutableReference(_) => "\"%p\"".to_string(),
            JophetType::Enum { .. } => "\"%d\"".to_string(),
            // The array itself is handled specially by the `println` logic, this is a fallback.
            JophetType::Array { .. } => "\"[Array]\"".to_string(),
            JophetType::UnsizedArray(_) => unreachable!("UnsizedArray type should not exist in the backend"),
            // Placeholder for any unprintable types.
            _ => "\"?\"".to_string(),
        }
    }
    
    /// Helper to determine if a type is a primitive that can be cloned with a simple copy.
    pub(super) fn is_primitive_for_clone(&self, jophet_type: &JophetType) -> bool {
        matches!(jophet_type,
            JophetType::Int(_)
            | JophetType::UInt(_)
            | JophetType::Float(_)
            | JophetType::Bool
            | JophetType::Char
            | JophetType::Enum { .. }
            | JophetType::USize
            | JophetType::ISize
        )
    }

    /// Helper to identify types that are aggregate structures in C.
    /// These are typically passed by value to user-defined functions but by pointer
    /// to C runtime functions for efficiency.
    pub(super) fn is_struct_like(&self, jophet_type: &JophetType) -> bool {
        matches!(jophet_type,
            JophetType::String
            | JophetType::Vector(_)
            | JophetType::Dictionary { .. }
            | JophetType::Struct { .. }
            | JophetType::Union { .. }
            | JophetType::TaggedUnion { .. }
            | JophetType::Error { .. }
            | JophetType::AnyError
            | JophetType::Tuple(_)
            | JophetType::Fallible { .. }
            | JophetType::Closure { .. }
        )
    }

    /// Recursively checks if a type is "cloneable".
    ///
    /// A type is cloneable if it's a primitive, an owned type like `String` or `Vector`,
    /// or a struct/tagged union where all fields/payloads are themselves cloneable.
    /// Pointers and references are not cloneable as they would lead to aliasing issues.
    pub fn type_is_cloneable(&mut self, jophet_type: &JophetType) -> bool {
        match jophet_type {
            JophetType::Int(_)
            | JophetType::UInt(_)
            | JophetType::Float(_)
            | JophetType::Bool
            | JophetType::Char
            | JophetType::USize
            | JophetType::ISize
            | JophetType::Enum { .. } => true,

            JophetType::String | JophetType::Vector(_) | JophetType::Closure { .. } => {
                self.runtime_needed = true;
                true
            },
            JophetType::Dictionary { key, value } => {
                self.runtime_needed = true;
                self.type_is_cloneable(key) && self.type_is_cloneable(value)
            },

            JophetType::Struct { name, .. } => self.cloneable_structs.contains(name),
            JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } => self.cloneable_tagged_unions.contains(name),
            
            JophetType::Array { member_type, .. } => self.type_is_cloneable(member_type),
            JophetType::Tuple(elements) => elements.iter().all(|t| self.type_is_cloneable(t)),

            // Pointers, references, and other types are not cloneable.
            _ => false,
        }
    }

    /// Generates the C expression for cloning a value of a given type.
    ///
    /// For primitives, this is just the value itself (pass-by-value copy).
    /// For owned types, it generates a call to the appropriate `_clone` function.
    /// For vectors, tuples, and arrays of owned types, it generates and caches a dedicated helper
    /// function or inline code to perform a deep, element-by-element clone.
    pub fn get_clone_call(&mut self, jophet_type: &JophetType, c_expr: &str) -> String {
        match jophet_type {
            JophetType::String => {
                self.runtime_needed = true;
                format!("String_clone(&{})", c_expr)
            }
            JophetType::Closure { .. } => {
                self.runtime_needed = true;
                format!("JophetClosure_clone(&{})", c_expr)
            }
            JophetType::Vector(member_type) => {
                self.runtime_needed = true;
                // If the vector's elements are primitives, a shallow copy (memcpy) is correct and efficient.
                if self.is_primitive_for_clone(member_type) {
                    format!("Vector_clone(&{})", c_expr)
                } else {
                    // For vectors of cloneable, non-primitive types (String, Struct, etc.),
                    // we must generate a helper function to perform a deep, element-wise clone.
                    let c_member_type = self.jophet_type_to_c_string(member_type);
                    let helper_name = format!("__jophet_clone_vector_of_{}", c_member_type.replace('*', "ptr"));

                    if !self.vector_clone_helpers.contains_key(&helper_name) {
                        let proto = format!("static JophetVector {}(const JophetVector* v);", helper_name);
                        self.function_prototypes.insert(proto);
                        
                        let mut helper_body = String::new();
                        // Recursively get the clone call for a single element.
                        let member_clone_call = self.get_clone_call(member_type, "&src_item");
                        
                        writeln!(&mut helper_body, "static JophetVector {}(const JophetVector* v) {{", helper_name).unwrap();
                        writeln!(&mut helper_body, "\tJophetVector new_v = Vector_new(v->elem_size);").unwrap();
                        writeln!(&mut helper_body, "\tif (v->len == 0) {{ return new_v; }}").unwrap();
                        writeln!(&mut helper_body, "\tfor (size_t i = 0; i < v->len; ++i) {{").unwrap();
                        writeln!(&mut helper_body, "\t\t{} src_item = ((({}*)v->data)[i]);", c_member_type, c_member_type).unwrap();
                        writeln!(&mut helper_body, "\t\t{} cloned_item = {};", c_member_type, member_clone_call).unwrap();
                        writeln!(&mut helper_body, "\t\tVector_push(&new_v, &cloned_item);").unwrap();
                        writeln!(&mut helper_body, "\t}}").unwrap();
                        writeln!(&mut helper_body, "\treturn new_v;").unwrap();
                        writeln!(&mut helper_body, "}}").unwrap();

                        self.vector_clone_helpers.insert(helper_name.clone(), helper_body);
                    }
                    format!("{}(&{})", helper_name, c_expr)
                }
            }
            JophetType::Dictionary { .. } => {
                self.runtime_needed = true;
                format!("Dictionary_clone(&{})", c_expr)
            }
            JophetType::Struct { name, .. } => {
                if self.cloneable_structs.contains(name) {
                    format!("{}_clone(&{})", name, c_expr)
                } else {
                    // This should be caught by the semantic analyzer, but as a fallback,
                    // we produce a simple copy which will likely cause a C compiler error
                    // for struct types, making the issue visible.
                    c_expr.to_string()
                }
            }
            JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } => {
                if self.cloneable_tagged_unions.contains(name) {
                    format!("{}_clone(&{})", name, c_expr)
                } else {
                    c_expr.to_string()
                }
            }
            JophetType::Tuple(elements) => {
                let c_type_name = self.jophet_type_to_c_string(jophet_type);
                let mut cloned_fields = Vec::new();

                for (i, element_type) in elements.iter().enumerate() {
                    let field_expr = format!("{}.f{}", c_expr, i);
                    let cloned_field = self.get_clone_call(element_type, &field_expr);
                    cloned_fields.push(cloned_field);
                }

                format!("({}){{ {} }}", c_type_name, cloned_fields.join(", "))
            }
            JophetType::Array { member_type, size } => {
                // If the array's elements are primitives, a simple copy is sufficient
                // and will be handled by the C assignment.
                if self.is_primitive_for_clone(member_type) {
                    return c_expr.to_string();
                }

                // Otherwise, we need to generate a loop to deep-clone each element.
                // This generates statements and returns a temporary variable name.
                let c_base_type = self.jophet_type_to_c_string(jophet_type);
                let dimension_suffix = self.get_array_dimension_suffix(jophet_type);

                let temp_array_var = format!("__clone_arr_{}", self.temp_var_counter);
                let loop_var = format!("__clone_idx_{}", self.temp_var_counter);
                self.temp_var_counter += 1;
                
                writeln!(&mut self.output, "\t{} {}{};", c_base_type, temp_array_var, dimension_suffix).unwrap();
                writeln!(&mut self.output, "\tfor (size_t {} = 0; {} < {}; ++{}) {{", loop_var, loop_var, size, loop_var).unwrap();
                
                let element_expr = format!("{}[{}]", c_expr, loop_var);
                let clone_call = self.get_clone_call(member_type, &element_expr);

                writeln!(&mut self.output, "\t\t{}[{}] = {};", temp_array_var, loop_var, clone_call).unwrap();
                writeln!(&mut self.output, "\t}}").unwrap();
                
                temp_array_var // The expression's value is the new temporary array.
            }
            // Primitives are copied by value.
            _ => c_expr.to_string(),
        }
    }

    /// Gets or creates a C helper function to perform a deep delete on a vector of a specific owned type.
    /// This function generates a C loop that calls the appropriate destructor on each element
    /// before freeing the vector's own data buffer.
    pub fn get_or_create_vector_deep_delete_helper(&mut self, member_type: &JophetType) -> String {
        let mangled_member_type = self.jophet_type_to_c_string_for_mangling(member_type);
        let helper_name = format!("__jophet_deep_delete_vector_of_{}", mangled_member_type);

        if self.vector_delete_helpers.contains_key(&helper_name) {
            return helper_name;
        }

        let c_member_type = self.jophet_type_to_c_string(member_type);
        
        let proto = format!("static void {}(JophetVector* v);", helper_name);
        self.function_prototypes.insert(proto);

        let mut helper_body = String::new();
        writeln!(&mut helper_body, "static void {}(JophetVector* v) {{", helper_name).unwrap();
        writeln!(&mut helper_body, "\tif (!v || !v->data) {{ return; }}").unwrap();
        writeln!(&mut helper_body, "\tfor (size_t i = 0; i < v->len; ++i) {{").unwrap();

        // The expression to access the i-th element's value.
        let element_expr = format!("((({}*)v->data)[i])", c_member_type);
        
        // Recursively get the cleanup call for a single element.
        // We pass `is_pointer=false` because `element_expr` is a value, and the
        // cleanup function (e.g., String_delete) expects to take its address.
        let element_cleanup_call = self.get_cleanup_call(member_type, &element_expr, false);
        
        writeln!(&mut helper_body, "\t\t{};", element_cleanup_call).unwrap();
        writeln!(&mut helper_body, "\t}}").unwrap();
        
        // After cleaning up all elements, perform the shallow delete of the vector's buffer.
        writeln!(&mut helper_body, "\tVector_delete(v);").unwrap();
        writeln!(&mut helper_body, "}}").unwrap();

        self.vector_delete_helpers.insert(helper_name.clone(), helper_body);
        helper_name
    }
    
    /// Gets or creates a C thunk function for deleting a dictionary item of a specific type.
    /// This is necessary because the generic dictionary runtime needs a `void (*)(void*)` function.
    pub fn get_or_create_item_delete_thunk(&mut self, ty: &JophetType) -> String {
        let mangled_type = self.jophet_type_to_c_string_for_mangling(ty);
        let thunk_name = format!("__jophet_delete_thunk_{}", mangled_type);

        if self.dictionary_delete_thunks.contains_key(&thunk_name) {
            return thunk_name;
        }

        let proto = format!("static void {}(void* data);", thunk_name);
        self.function_prototypes.insert(proto);

        let c_type = self.jophet_type_to_c_string(ty);
        // The cleanup call takes a pointer, so we pass `data` directly.
        let cleanup_call = self.get_cleanup_call(ty, "data", true);

        let mut def = String::new();
        writeln!(&mut def, "static void {}(void* data) {{", thunk_name).unwrap();
        writeln!(&mut def, "\t{};", cleanup_call).unwrap();
        writeln!(&mut def, "}}").unwrap();

        self.dictionary_delete_thunks.insert(thunk_name.clone(), def);
        thunk_name
    }
    
    /// Gets or creates a C thunk function for cloning a dictionary item of a specific type.
    /// This is necessary because the generic dictionary runtime needs a `void* (*)(const void*)` function.
    pub fn get_or_create_item_clone_thunk(&mut self, ty: &JophetType) -> String {
        let mangled_type = self.jophet_type_to_c_string_for_mangling(ty);
        let thunk_name = format!("__jophet_clone_thunk_{}", mangled_type);

        if self.dictionary_clone_thunks.contains_key(&thunk_name) {
            return thunk_name;
        }

        let proto = format!("static void* {}(const void* data);", thunk_name);
        self.function_prototypes.insert(proto);

        let c_type = self.jophet_type_to_c_string(ty);
        let clone_call = self.get_clone_call(ty, &format!("*(const {}*)data", c_type));
        
        let mut def = String::new();
        writeln!(&mut def, "static void* {}(const void* data) {{", thunk_name).unwrap();
        writeln!(&mut def, "\t{}* new_obj = ({0}*)malloc(sizeof({0}));", c_type).unwrap();
        writeln!(&mut def, "\t*new_obj = {};", clone_call).unwrap();
        writeln!(&mut def, "\treturn new_obj;").unwrap();
        writeln!(&mut def, "}}").unwrap();
        
        self.dictionary_clone_thunks.insert(thunk_name.clone(), def);
        thunk_name
    }

    /// Recursively finds the base (innermost) member type of a potentially multi-dimensional array.
    pub fn get_array_base_type<'a>(&self, mut t: &'a JophetType) -> &'a JophetType {
        while let JophetType::Array { member_type, .. } = t {
            t = member_type;
        }
        t
    }

    /// Recursively calculates the total number of elements in a potentially multi-dimensional array.
    pub fn get_array_total_size(&self, t: &JophetType) -> usize {
        if let JophetType::Array { member_type, size } = t {
            size * self.get_array_total_size(member_type)
        } else {
            1
        }
    }
}