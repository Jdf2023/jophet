// src/backend/c/expressions/access.rs
//! Handles compilation of expressions that access parts of a data structure.
//! This includes field access (`.`), tuple access (`.N`), and array/vector indexing (`[]`).

use super::super::Generator;
use super::CExpression;
use crate::core::ast::typed::*;

impl Generator {
    /// Compiles a field access expression (e.g., `obj.field`).
    /// It correctly uses `->` for pointers and references, and `.` for values.
    pub(super) fn compile_field_access_expression(
        &mut self,
        object: &TypedExpression,
        field: &str,
    ) -> CExpression {
        // A C pointer is used for Jophet Pointers, References, and MutableReferences.
        let is_c_pointer = matches!(
            object.jophet_type,
            JophetType::Pointer(_) | JophetType::Reference(_) | JophetType::MutableReference(_)
        );
        let operator = if is_c_pointer { "->" } else { "." };
        let result = format!(
            "{}{}{}",
            self.compile_expression(object),
            operator,
            self.sanitize_c_keyword(field)
        );
        CExpression::Simple(result)
    }

    /// Compiles a tuple access expression (e.g., `my_tuple.0`).
    pub(super) fn compile_tuple_access_expression(
        &mut self,
        tuple_expr: &TypedExpression,
        index: usize,
    ) -> CExpression {
        let result = format!("{}.f{}", self.compile_expression(tuple_expr), index);
        CExpression::Simple(result)
    }

    /// Compiles an array or vector indexing expression into its C equivalent.
    ///
    /// If `use_bounds_check` is true, it is wrapped with a runtime bounds check
    /// that includes source location info. Otherwise, a raw C index is generated.
    /// It creates a temporary variable for vector/array r-values to prevent double evaluation.
    pub(super) fn compile_array_index_expression(
        &mut self,
        array: &TypedExpression,
        index: &TypedExpression,
        size: Option<usize>,
        span_start: usize,
        use_bounds_check: bool,
    ) -> CExpression {
        // 1. Immutable borrows first.
        let line = self.source_map.line_for_byte(span_start);
        let file = self.source_map.filename().to_string(); // Clone to release borrow.

        // 2. Mutable borrows second.
        let compiled_index = self.compile_expression(index);
        let bounds_check_helper = if use_bounds_check {
            self.get_bounds_check_helper()
        } else {
            "" // No helper function for unchecked access
        };

        // 3. Combine results.
        let result = match &array.jophet_type {
            JophetType::Array { .. } => {
                let size_val =
                    size.expect("Array must have a compile-time size for bounds checking");
                // For arrays, the expression is usually already an l-value.
                let compiled_array = self.compile_expression(array);
                
                if use_bounds_check {
                    format!(
                        "({array})[{bounds_check}({index}, {size}, \"{file}\", {line})]",
                        array = compiled_array,
                        bounds_check = bounds_check_helper,
                        index = compiled_index,
                        size = size_val,
                        file = file,
                        line = line
                    )
                } else {
                    format!(
                        "({array})[{index}]",
                        array = compiled_array,
                        index = compiled_index
                    )
                }
            }
            JophetType::Vector(member_type) => {
                // Use `ensure_lvalue` to prevent double evaluation of the vector expression.
                let compiled_array_lvalue = self.ensure_lvalue(array);
                let c_member_type = self.jophet_type_to_c_string(member_type);

                if use_bounds_check {
                    format!("(({c_type}*)({vec}).data)[{bounds_check}({index}, ({vec}).len, \"{file}\", {line})]",
                        c_type = c_member_type,
                        bounds_check = bounds_check_helper,
                        vec = compiled_array_lvalue,
                        index = compiled_index,
                        file = file,
                        line = line
                    )
                } else {
                    format!("(({c_type}*)({vec}).data)[{index}]",
                        c_type = c_member_type,
                        vec = compiled_array_lvalue,
                        index = compiled_index,
                    )
                }
            }
            _ => unreachable!("Semantic analysis should prevent indexing on non-indexable types."),
        };
        CExpression::Simple(result)
    }

    /// Compiles an array, vector, or string slicing expression into a call to a C runtime helper.
    ///
    /// This function translates a slice operation into a call to one of three C runtime functions:
    /// - `jophet_string_slice` for `String` and `StringSlice` types.
    /// - `jophet_slice_shallow` for collections of primitive types, which performs a fast `memcpy`.
    /// - `jophet_slice_deep` for collections of owned types, which performs an element-by-element clone.
    /// It creates a temporary variable for vector/string r-values to prevent double evaluation.
    pub(super) fn compile_array_slice_expression(
        &mut self,
        array: &TypedExpression,
        start: &Option<Box<TypedExpression>>,
        end: &Option<Box<TypedExpression>>,
        span_start: usize,
    ) -> CExpression {
        self.runtime_needed = true;

        // 1. Perform all immutable borrows first and store the results.
        let line = self.source_map.line_for_byte(span_start);
        let file = self.source_map.filename().to_string(); // Clone to release the borrow on `self`

        // 2. Perform all mutable borrows.
        let compiled_start = start.as_ref().map_or("0".to_string(), |s| self.compile_expression(s));
        
        let (data_ptr, len_expr, elem_size_expr, member_type) = match &array.jophet_type {
            JophetType::Array { member_type, size } => {
                let compiled_array = self.compile_expression(array);
                (
                    compiled_array.clone(),
                    size.to_string(),
                    format!("sizeof({})", self.jophet_type_to_c_string(member_type)),
                    Some(member_type.as_ref())
                )
            }
            JophetType::Vector(member_type) => {
                let compiled_array_lvalue = self.ensure_lvalue(array);
                (
                    format!("{}.data", compiled_array_lvalue),
                    format!("{}.len", compiled_array_lvalue),
                    format!("{}.elem_size", compiled_array_lvalue),
                    Some(member_type.as_ref())
                )
            }
            JophetType::String => {
                let compiled_array_lvalue = self.ensure_lvalue(array);
                (
                    format!("{}.data", compiled_array_lvalue),
                    format!("{}.len", compiled_array_lvalue),
                    "sizeof(char)".to_string(),
                    None
                )
            }
             JophetType::StringSlice => {
                let compiled_array = self.compile_expression(array);
                (
                    compiled_array.clone(),
                    format!("strlen({})", compiled_array),
                    "sizeof(char)".to_string(),
                    None
                )
            }
            _ => unreachable!("Semantic analysis should prevent slicing non-sliceable types."),
        };

        let compiled_end = end.as_ref().map_or(len_expr.clone(), |e| self.compile_expression(e));

        let (slice_fn, clone_fn_arg) = if let Some(m_type) = member_type {
            if self.type_is_cloneable(m_type) && !self.is_primitive_for_clone(m_type) {
                let clone_thunk_name = self.get_or_create_item_clone_thunk(m_type);
                (
                    "jophet_slice_deep".to_string(),
                    format!("&{}", clone_thunk_name),
                )
            } else {
                ("jophet_slice_shallow".to_string(), "NULL".to_string())
            }
        } else {
            // For strings, the clone function is not needed.
            ("jophet_string_slice".to_string(), "NULL".to_string())
        };

        // 3. Combine the results.
        let result = format!(
            "{slice_fn}({data_ptr}, {len}, {elem_size}, {start}, {end}, {clone_fn}, \"{file}\", {line})",
            slice_fn = slice_fn,
            data_ptr = data_ptr,
            len = len_expr,
            elem_size = elem_size_expr,
            start = compiled_start,
            end = compiled_end,
            clone_fn = clone_fn_arg,
            file = file,
            line = line
        );

        CExpression::Simple(result)
    }
}