// src/backend/c/expressions/mod.rs
//! The main dispatcher for compiling Jophet expressions into C.
//!
//! This module re-exports and orchestrates the various sub-modules that handle
//! specific categories of expression compilation.

use super::Generator;
use crate::core::ast::typed::*;

pub mod access;
pub mod calls;
pub mod control_flow;
pub mod helpers;
pub mod instantiation;
pub mod literals;

/// The result of compiling a Jophet expression into C.
///
/// This enum is used internally by the compiler to distinguish between two cases:
/// 1. `Simple(String)`: The expression translates to a straightforward C expression string (e.g., "1 + 2").
/// 2. `Temporary(String)`: The expression is complex and required generating pre-statements
///    and storing the result in a temporary C variable. The string is the name of that variable.
///
/// This distinction is crucial for callers like `catch` to avoid creating redundant temporary variables.
pub(super) enum CExpression {
    Simple(String),
    Temporary(String),
}

impl CExpression {
    /// Consumes the enum and returns the underlying C code string, whether it's
    /// a simple expression or a temporary variable name.
    pub(super) fn into_string(self) -> String {
        match self {
            CExpression::Simple(s) => s,
            CExpression::Temporary(s) => s,
        }
    }
}

impl Generator {
    /// Compiles a Jophet `TypedExpression` into a C expression string.
    ///
    /// This is the main public entry point for expression compilation for most of the compiler.
    /// It calls `compile_expression_internal` and returns the final C code as a simple string.
    pub fn compile_expression(&mut self, expr: &TypedExpression) -> String {
        self.compile_expression_internal(expr).into_string()
    }

    /// The internal implementation for compiling a Jophet `TypedExpression`.
    ///
    /// This is the core dispatcher that returns a structured `CExpression`, allowing callers
    /// to know if a temporary variable was generated. It now handles `convert` expressions
    /// from `PythonObject` to various native types (`Array<T, N>`, `Vector<Vector<T>>`,
    /// `Vector<PythonObject>`, structs, tuples, etc.) by generating calls to new helper functions.
    /// It also handles the re-branding of `PythonObject` types. A `ConstCall` is impossible
    /// here, as the semantic analyzer replaces them with literals. It now also correctly
    /// generates C code for large `u64` literals using the `ULL` suffix.
    pub(super) fn compile_expression_internal(&mut self, expr: &TypedExpression) -> CExpression {
        match &expr.kind {
            TypedExpressionKind::Error => CExpression::Simple("/* ERROR */".to_string()),
            TypedExpressionKind::New { jophet_type, args } => self.compile_new_expression(jophet_type, args),
            TypedExpressionKind::Literal(lit) => self.compile_literal_expression(lit, &expr.jophet_type),
            TypedExpressionKind::UInt64Literal(u) => CExpression::Simple(format!("{}ULL", u)),
            TypedExpressionKind::Identifier { name, mangled_name } => {
                let identifier_str = if let Some(mangled) = mangled_name {
                    mangled.clone()
                } else {
                    let sanitized_name = self.sanitize_c_keyword(name);
                    if self.current_closure_captures.contains(name) {
                        format!("env->{}", sanitized_name)
                    } else {
                        sanitized_name
                    }
                };
                CExpression::Simple(identifier_str)
            }
            TypedExpressionKind::EnumVariantAccess { enum_name, variant_name, .. } => CExpression::Simple(format!("{}_{}", enum_name, variant_name)),
            TypedExpressionKind::UnaryOp(op, right) => self.compile_unary_op_expression(op, right),
            TypedExpressionKind::TernaryOp(cond, then, else_b) => self.compile_ternary_op_expression(cond, then, else_b),
            TypedExpressionKind::BinaryOp(left, op, right) => self.compile_binary_op_expression(left, op, right, expr.span.start),
            TypedExpressionKind::Closure { function, captures } => self.compile_closure_expression(function, captures),
            TypedExpressionKind::FunctionCall { kind, args } => self.compile_function_call_expression(kind, args, expr),
            TypedExpressionKind::ConstCall { .. } => unreachable!("`const` calls should be replaced with literals by the semantic analyzer and should not reach the backend."),
            TypedExpressionKind::InterpolatedString(parts) => self.compile_interpolated_string_expression(parts),
            TypedExpressionKind::StructInstantiation(name, args) => self.compile_struct_instantiation_expression(name, args),
            TypedExpressionKind::UnionInstantiation { union_name, field_name, value } => self.compile_union_instantiation_expression(union_name, field_name, value),
            TypedExpressionKind::TaggedUnionInstantiation { enum_name, variant_name, payload } => self.compile_tagged_union_instantiation(enum_name, variant_name, payload),
            TypedExpressionKind::FieldAccess(object, field) => self.compile_field_access_expression(object, field),
            TypedExpressionKind::MethodCall { object, mangled_name, args } => self.compile_method_call_expression(object, mangled_name, args, &expr.jophet_type),
            TypedExpressionKind::Tuple(elements) => self.compile_tuple_expression(elements, &expr.jophet_type),
            TypedExpressionKind::TupleAccess(tuple_expr, index) => self.compile_tuple_access_expression(tuple_expr, *index),
            TypedExpressionKind::AddressOf(addr_expr) => CExpression::Simple(format!("&{}", self.compile_expression(addr_expr))),
            TypedExpressionKind::Dereference(deref_expr) => CExpression::Simple(format!("*{}", self.compile_expression(deref_expr))),
            TypedExpressionKind::ArrayLiteral(elements) => self.compile_array_literal_expression(elements),
            TypedExpressionKind::ArrayIndex { array, index, size } => self.compile_array_index_expression(array, index, *size, expr.span.start, true),
            TypedExpressionKind::ArraySlice { array, start, end, size: _ } => self.compile_array_slice_expression(array, start, end, expr.span.start),
            TypedExpressionKind::DictionaryInstantiation { key_type, value_type, pairs } => self.compile_dictionary_instantiation_expression(key_type, value_type, pairs),
            TypedExpressionKind::Switch { expression, cases, else_block } => self.compile_switch_expression(&expr.jophet_type, expression, cases, else_block),
            TypedExpressionKind::PropagateError { expr: propagate_expr } => self.compile_propagate_error_expression(propagate_expr),
            TypedExpressionKind::UnwrapOrPanic { expr: unwrap_expr } => self.compile_unwrap_or_panic_expression(unwrap_expr),
            TypedExpressionKind::Catch { expression, error_variable, body } => self.compile_catch_expression(&expr.jophet_type, expression, error_variable, body),
            TypedExpressionKind::FallibleWrap { is_ok, expr: inner_expr } => self.compile_fallible_wrap_expression(&expr.jophet_type, *is_ok, inner_expr),
            TypedExpressionKind::ErrorUpcast { expr: inner_expr } => self.compile_error_upcast_expression(inner_expr),
            TypedExpressionKind::Allow(inner_expr) => self.compile_expression_internal(inner_expr),
            TypedExpressionKind::Convert { expr, target_type } => {
                let c_target_type = self.jophet_type_to_c_string(target_type);
                let compiled_inner = self.compile_expression(expr);
                
                // Handle the three main conversion scenarios
                match (&expr.jophet_type, target_type) {
                    // 1. PythonObject -> NativeJophetType (Extraction)
                    (JophetType::PythonObject { .. }, native_type) if !matches!(native_type, JophetType::PythonObject { .. }) => {
                        self.python_runtime_needed = true;
                        if let JophetType::Array { member_type, size } = target_type {
                            let helper_name = self.get_or_create_py_to_array_helper(member_type, *size);
                            return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                        }
                        if let JophetType::Vector(member_type) = target_type {
                            if let JophetType::Vector(inner_member_type) = member_type.as_ref() {
                                let helper_name = self.get_or_create_py_to_vector_vector_helper(inner_member_type);
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                            // Handle the conversion from a Python list to a Vector<PythonObject>.
                            if let JophetType::PythonObject { .. } = member_type.as_ref() {
                                let helper_name = self.get_or_create_py_to_vector_python_object_helper();
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                        }
                        let mangled_target_type = match target_type {
                            JophetType::Int(8) => "i8".to_string(),
                            JophetType::Int(16) => "i16".to_string(),
                            JophetType::Int(32) => "i32".to_string(),
                            JophetType::Int(64) => "i64".to_string(),
                            JophetType::UInt(8) => "u8".to_string(),
                            JophetType::UInt(16) => "u16".to_string(),
                            JophetType::UInt(32) => "u32".to_string(),
                            JophetType::UInt(64) => "u64".to_string(),
                            JophetType::Float(32) => "f32".to_string(),
                            JophetType::Float(64) => "f64".to_string(),
                            JophetType::Char => "char".to_string(),
                            JophetType::Enum { name, .. } => {
                                let helper_name = self.get_or_create_py_to_enum_helper(name);
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                            JophetType::Tuple(element_types) => {
                                let helper_name = self.get_or_create_py_to_tuple_helper(element_types);
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                            JophetType::Struct { name, .. } => {
                                let helper_name = self.get_or_create_py_to_struct_helper(name);
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                            JophetType::Dictionary { key, value } => {
                                let helper_name = self.get_or_create_py_to_dictionary_helper(key, value);
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                            JophetType::TaggedUnion { name, .. } | JophetType::Error { name, .. } => {
                                let helper_name = self.get_or_create_py_to_tagged_union_helper(name);
                                return CExpression::Simple(format!("{}({})", helper_name, compiled_inner));
                            }
                            _ => self.jophet_type_to_c_string_for_mangling(target_type),
                        };
                        let conversion_fn = format!("jophet_py_convert_to_{}", mangled_target_type);
                        return CExpression::Simple(format!("{}({})", conversion_fn, compiled_inner));
                    }
                    // 2. PythonObject -> PythonObject (Re-branding)
                    (JophetType::PythonObject { .. }, JophetType::PythonObject { .. }) => {
                        // This is a static cast, so it generates no C code.
                        // The cast is purely for the Jophet type system.
                        CExpression::Simple(format!("({})", compiled_inner))
                    }
                    // 3. All other conversions (e.g., int to float)
                    _ => CExpression::Simple(format!("({})({})", c_target_type, compiled_inner)),
                }
            },
            TypedExpressionKind::Clone(inner) => {
                let compiled_inner = self.compile_expression(inner);
                // `get_clone_call` can sometimes generate statements (for array clones),
                // in which case it returns a temporary variable name. We must treat it as such.
                if compiled_inner.starts_with("__clone_arr_") {
                    CExpression::Temporary(compiled_inner)
                } else {
                    CExpression::Simple(self.get_clone_call(&inner.jophet_type, &compiled_inner))
                }
            }
            TypedExpressionKind::Parse { target_type, expr: parse_expr } => self.compile_parse_expression(target_type, parse_expr),
            TypedExpressionKind::IncludeC { header } => {
                self.c_ffi_headers.insert(header.clone());
                // The expression evaluates to a conceptual handle, but there's no C value.
                // We initialize the handle variable to NULL.
                CExpression::Simple("(void*)0".to_string())
            },
            TypedExpressionKind::ImportPy { module_name } => {
                self.python_runtime_needed = true;
                CExpression::Simple(format!("jophet_py_import(\"{}\")", module_name))
            },
        }
    }
}