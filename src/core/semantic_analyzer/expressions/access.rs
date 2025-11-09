// src/core/semantic_analyzer/expressions/access.rs
//! Handles member, index, and path access analysis.
//!
//! This module includes compile-time bounds helpers for arrays where possible.
//! `const` initializer validation is handled in declaration analysis.
//!
//! This module handles the analysis of field access (`.`), tuple access (`.N`),
//! array/vector indexing (`[]`), and enum variant access (`Enum.Variant`). It has
//! been updated to work with the error-collecting paradigm by passing a mutable
//! error vector to sub-analyzers and propagating error sentinels.

use crate::core::ast::typed::*;
use crate::core::ast::untyped;
use crate::core::ast::Literal;
use crate::core::semantic_analyzer::{types::jophet_type_to_user_string, ScopeContext, SemanticAnalyzer};
use crate::diagnostics::errors::SemanticError;

impl SemanticAnalyzer<'_> {
    /// Analyzes a field access for a module, struct, union, or Python object.
    /// It now handles `module.member`, native struct fields, and Python attribute access.
    pub fn analyze_field_access_expr(
        &mut self,
        object: &untyped::Expression,
        field_name: &str,
        ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_object = self.analyze_expression(object, ctx, None, errors);
        if typed_object.jophet_type == JophetType::ErrorSentinel {
            return Ok(typed_object);
        }
        let span = object.span.clone();

        // Handle PythonObject attribute access
        if let JophetType::PythonObject { .. } | JophetType::PythonModule = &typed_object.jophet_type {
            self.needs_python_runtime = true;
            // The "attribute" is passed as a string literal argument to a magic method.
            let attribute_arg = TypedExpression {
                kind: TypedExpressionKind::Literal(Literal::String(field_name.to_string())),
                jophet_type: JophetType::StringSlice,
                span: span.clone(), // Approximate span
            };

            // We represent this special operation as a method call with a "magic" name.
            // The backend will know how to handle it.
            return Ok(TypedExpression {
                kind: TypedExpressionKind::MethodCall {
                    object: Box::new(typed_object),
                    mangled_name: "__getattr__".to_string(),
                    args: vec![attribute_arg],
                },
                // The result of a Python operation is always another opaque PythonObject,
                // which we brand as PyAny by default.
                jophet_type: JophetType::PythonObject { brand: Box::new(self.py_any_brand.clone()) },
                span,
            });
        }

        // Handle module member access: `my_module.some_function` or `my_module.SomeType`
        if let JophetType::Module { name: mod_name } = &typed_object.jophet_type {
            let module_scope = self.modules.get(mod_name).ok_or_else(|| {
                SemanticError::InternalError {
                    message: format!("Module '{}' was not loaded correctly.", mod_name),
                    span: object.span.clone(),
                    file_path: self.current_module_path.clone(),
                }
            })?;

            // Look for a public function or type in the module's scope.
            if let Some(info) = module_scope.symbol_table.get(field_name) {
                // It's a function or variable. We return an Identifier expression with its mangled name.
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::Identifier {
                        name: field_name.to_string(),
                        mangled_name: info.mangled_name.clone(),
                    },
                    jophet_type: info.jophet_type.clone(),
                    span,
                });
            }

            // This is not a standard expression, but a type access. We can represent it as
            // an identifier whose type is the struct/enum/etc. itself.
            let resolved_type =
                self.resolve_type(&untyped::Type::Simple(field_name.to_string()), false, None, ctx, span.clone());
            if let Ok(ty) = resolved_type {
                return Ok(TypedExpression {
                    kind: TypedExpressionKind::Identifier {
                        name: field_name.to_string(),
                        mangled_name: None,
                    },
                    jophet_type: ty,
                    span,
                });
            }

            return Err(SemanticError::NameError {
                message: format!("Module '{}' has no public member named '{}'", mod_name, field_name),
                span,
                file_path: self.current_module_path.clone(),
            });
        }

        let object_type = match typed_object.jophet_type.clone() {
            JophetType::Pointer(inner)
            | JophetType::Reference(inner)
            | JophetType::MutableReference(inner) => *inner,
            _ => typed_object.jophet_type.clone(),
        };

        match &object_type {
            JophetType::Struct { name, module_path } => {
                let (jophet_type, is_public) =
                    if let Some(struct_def) = self.struct_defs.get(name).cloned() {
                        let (field_type_untyped, is_public) = struct_def
                            .fields
                            .iter()
                            .find(|(fname, _, _, _)| fname == field_name)
                            .map(|(_, ftype, fpub, _)| (ftype, *fpub))
                            .ok_or_else(|| SemanticError::NameError {
                                message: format!("No such field '{}' on struct '{}'", field_name, name),
                                span: span.clone(),
                                file_path: self.current_module_path.clone(),
                            })?;
                        (
                            self.resolve_type(field_type_untyped, true, Some(name), ctx, span.clone())?,
                            is_public,
                        )
                    } else {
                        let imported_struct_def = self
                            .modules
                            .values()
                            .find_map(|module_scope| module_scope.struct_defs.get(name))
                            .ok_or_else(|| SemanticError::NameError {
                                message: format!("Definition for imported struct '{}' not found", name),
                                span: span.clone(),
                                file_path: self.current_module_path.clone(),
                            })?;
                        let (field_type_typed, is_public) = imported_struct_def
                            .fields
                            .iter()
                            .find(|(fname, _, _)| fname == field_name)
                            .map(|(_, ftype, fpub)| (ftype.clone(), *fpub))
                            .ok_or_else(|| SemanticError::NameError {
                                message: format!("No such field '{}' on struct '{}'", field_name, name),
                                span: span.clone(),
                                file_path: self.current_module_path.clone(),
                            })?;
                        (field_type_typed, is_public)
                    };

                if self.current_module_path != *module_path && !is_public {
                    return Err(SemanticError::NameError {
                        message: format!("Field '{}' of struct '{}' is private", field_name, name),
                        span,
                        file_path: self.current_module_path.clone(),
                    });
                }

                Ok(TypedExpression {
                    kind: TypedExpressionKind::FieldAccess(
                        Box::new(typed_object),
                        field_name.to_string(),
                    ),
                    jophet_type,
                    span,
                })
            }
            JophetType::Union { name, .. } => {
                let union_def = self.union_defs.get(name).cloned().ok_or_else(|| SemanticError::InternalError { message: format!("Union definition for '{}' not found", name), span: span.clone(), file_path: self.current_module_path.clone()})?;
                let (field_type_untyped,) = union_def
                    .fields
                    .iter()
                    .find(|(fname, _, _)| fname == field_name)
                    .map(|(_, ftype, _)| (ftype,))
                    .ok_or_else(|| SemanticError::NameError {
                        message: format!("No such field '{}' on union '{}'", field_name, name),
                        span: span.clone(),
                        file_path: self.current_module_path.clone(),
                    })?;

                let jophet_type =
                    self.resolve_type(field_type_untyped, false, None, ctx, span.clone())?;

                Ok(TypedExpression {
                    kind: TypedExpressionKind::FieldAccess(
                        Box::new(typed_object),
                        field_name.to_string(),
                    ),
                    jophet_type,
                    span,
                })
            }
            _ => Err(SemanticError::TypeError {
                message: format!(
                    "Field access on non-aggregate type '{}'",
                    jophet_type_to_user_string(&object_type)
                ),
                span,
                file_path: self.current_module_path.clone(),
            }),
        }
    }

    /// Analyzes a tuple access (e.g., `my_tuple.0`), checking for out-of-bounds errors.
    pub fn analyze_tuple_access_expr(
        &mut self,
        object: &untyped::Expression,
        index: usize,
        ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_object = self.analyze_expression(object, ctx, None, errors);
        if typed_object.jophet_type == JophetType::ErrorSentinel { return Ok(typed_object); }
        let span = object.span.clone();
        match &typed_object.jophet_type {
            JophetType::Tuple(types) => {
                let element_type =
                    types
                        .get(index)
                        .ok_or_else(|| SemanticError::TypeError {
                            message: format!("Tuple index {} is out of bounds", index),
                            span: span.clone(),
                            file_path: self.current_module_path.clone(),
                        })?
                        .clone();
                Ok(TypedExpression {
                    kind: TypedExpressionKind::TupleAccess(Box::new(typed_object), index),
                    jophet_type: element_type,
                    span,
                })
            }
            _ => Err(SemanticError::TypeError {
                message: "Tuple access on a non-tuple type".to_string(),
                span,
                file_path: self.current_module_path.clone(),
            }),
        }
    }

    /// Analyzes an array, vector, or Python object indexing expression.
    /// It checks that the index is an integer or string (for Python) and performs
    /// compile-time bounds checking for arrays with constant indices. It now transforms
    /// indexing on a `PythonObject` into a `__getitem__` method call.
    pub fn analyze_array_index_expr(
        &mut self,
        array: &untyped::Expression,
        index: &untyped::Expression,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_array = self.analyze_expression(array, ctx, None, errors);
        if typed_array.jophet_type == JophetType::ErrorSentinel { return Ok(typed_array); }

        // Handle PythonObject indexing
        if let JophetType::PythonObject {..} = typed_array.jophet_type {
            self.needs_python_runtime = true;
            let typed_index = self.analyze_expression(index, ctx, None, errors);
            if typed_index.jophet_type == JophetType::ErrorSentinel { return Ok(typed_index); }

            // Python's __getitem__ is very flexible. We'll allow integers and strings.
            if !matches!(typed_index.jophet_type, JophetType::Int(_) | JophetType::UInt(_) | JophetType::String | JophetType::StringSlice) {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Python object key must be an integer or string, but found type '{}'",
                        jophet_type_to_user_string(&typed_index.jophet_type)
                    ),
                    span: index.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }

            // We represent this special operation as a method call with a "magic" name.
            // The backend will know how to handle it.
            return Ok(TypedExpression {
                kind: TypedExpressionKind::MethodCall {
                    object: Box::new(typed_array),
                    mangled_name: "__getitem__".to_string(),
                    args: vec![typed_index],
                },
                // The result of a Python operation is always another opaque PythonObject.
                jophet_type: JophetType::PythonObject { brand: Box::new(self.py_any_brand.clone()) },
                span,
            });
        }

        let typed_index = self.analyze_expression(index, ctx, Some(&JophetType::Int(64)), errors);
        if typed_index.jophet_type == JophetType::ErrorSentinel { return Ok(typed_index); }

        if !matches!(typed_index.jophet_type, JophetType::Int(_) | JophetType::UInt(_)) {
            return Err(SemanticError::TypeError {
                message: format!(
                    "Array or Vector index must be an integer, but found type {}",
                    jophet_type_to_user_string(&typed_index.jophet_type)
                ),
                span: index.span.clone(),
                file_path: self.current_module_path.clone(),
            });
        }

        let (member_type, size) = match &typed_array.jophet_type {
            JophetType::Array { member_type, size } => (member_type.as_ref().clone(), Some(*size)),
            JophetType::Vector(member_type) => (member_type.as_ref().clone(), None),
            _ => {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Cannot index into a value of type {}",
                        jophet_type_to_user_string(&typed_array.jophet_type)
                    ),
                    span: array.span.clone(),
                    file_path: self.current_module_path.clone(),
                })
            }
        };

        // Perform compile-time bounds check if possible.
        if let (Some(array_size), TypedExpressionKind::Literal(Literal::Int(index_val))) =
            (size, &typed_index.kind)
        {
            if *index_val < 0 || (*index_val as usize) >= array_size {
                return Err(SemanticError::MemoryError {
                    message: format!(
                        "Index out of bounds: the len is {} but the index is {}",
                        array_size, index_val
                    ),
                    span: index.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        }

        Ok(TypedExpression {
            kind: TypedExpressionKind::ArrayIndex {
                array: Box::new(typed_array),
                index: Box::new(typed_index),
                size,
            },
            jophet_type: member_type,
            span,
        })
    }

    /// Analyzes an enum, tagged union, or error variant access (e.g., `MyEnum.Variant`).
    /// This is now the definitive logic that disambiguates between C-style enums and
    /// payload-less tagged unions, producing a different typed AST node for each.
    pub fn analyze_enum_variant_access_expr(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        // Priority 1: Check if it's a standard C-style enum.
        if let Some(enum_def) = self.enum_defs.get(enum_name) {
            if !enum_def
                .members
                .iter()
                .any(|(name, _, _)| name == variant_name)
            {
                return Err(SemanticError::NameError {
                    message: format!("Enum '{}' has no variant '{}'", enum_name, variant_name),
                    span,
                    file_path: self.current_module_path.clone(),
                });
            }

            let mut typed_members = Vec::new();
            let mut next_value = 0i64;
            for (name, value_opt, doc) in &enum_def.members {
                let current_value = match value_opt {
                    Some(val) => *val,
                    None => next_value,
                };
                typed_members.push((name.clone(), current_value, doc.clone()));
                next_value = current_value + 1;
            }

            let enum_type = JophetType::Enum {
                name: enum_def.name.clone(),
                members: typed_members,
                module_path: enum_def.module_path.clone(),
            };

            return Ok(TypedExpression {
                kind: TypedExpressionKind::EnumVariantAccess {
                    enum_name: enum_name.to_string(),
                    variant_name: variant_name.to_string(),
                },
                jophet_type: enum_type,
                span,
            });
        }

        // Priority 2 & 3: Check if it's a tagged union or error variant with no payload.
        // If so, it's an instantiation, not a simple access.
        if self.tagged_union_defs.contains_key(enum_name) || self.error_defs.contains_key(enum_name)
        {
            return self.analyze_tagged_union_instantiation_expr(
                enum_name,
                variant_name,
                &None, // Explicitly no payload
                ctx,
                span,
                errors,
            );
        }

        // If not found in any of the relevant definition maps, then it's an error.
        Err(SemanticError::NameError {
            message: format!(
                "No such enum, tagged union, or error type named '{}'",
                enum_name
            ),
            span: span.clone(),
            file_path: self.current_module_path.clone(),
        })
    }

    /// Analyzes an array, string, or `PythonObject` slicing expression (e.g., `my_array[start:end]`).
    ///
    /// It validates that the object is sliceable, analyzes the start and end bound
    /// expressions, and determines the result type of the slice. It now transforms
    /// slicing on a `PythonObject` into a `__getitem__` method call with a `slice` object.
    /// It also correctly handles the special `begin` and `end` identifiers as slice bounds.
    pub fn analyze_array_slice_expr(
        &mut self,
        array: &untyped::Expression,
        start: &Option<Box<untyped::Expression>>,
        end: &Option<Box<untyped::Expression>>,
        ctx: &mut ScopeContext,
        span: crate::core::ast::Span,
        errors: &mut Vec<SemanticError>,
    ) -> Result<TypedExpression, SemanticError> {
        let typed_array = self.analyze_expression(array, ctx, None, errors);
        if typed_array.jophet_type == JophetType::ErrorSentinel {
            return Ok(typed_array);
        }

        // Analyze the `start` expression if it exists, handling the special 'begin' identifier.
        let typed_start = match start {
            Some(s_expr) => {
                if let untyped::ExpressionKind::Identifier(name) = &s_expr.kind {
                    if name == "begin" {
                        None // Treat 'begin' as an omitted start bound.
                    } else {
                        // It's a regular variable/expression, analyze normally.
                        let typed_s = self.analyze_expression(s_expr, ctx, Some(&JophetType::UInt(64)), errors);
                        if !matches!(typed_s.jophet_type, JophetType::Int(_) | JophetType::UInt(_)) {
                            errors.push(SemanticError::TypeError {
                                message: "Slice start index must be an integer.".to_string(),
                                span: s_expr.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                        Some(Box::new(typed_s))
                    }
                } else {
                    let typed_s = self.analyze_expression(s_expr, ctx, Some(&JophetType::UInt(64)), errors);
                    if !matches!(typed_s.jophet_type, JophetType::Int(_) | JophetType::UInt(_)) {
                        errors.push(SemanticError::TypeError {
                            message: "Slice start index must be an integer.".to_string(),
                            span: s_expr.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    Some(Box::new(typed_s))
                }
            }
            None => None,
        };

        // Analyze the `end` expression if it exists, handling the special 'end' identifier.
        let typed_end = match end {
            Some(e_expr) => {
                if let untyped::ExpressionKind::Identifier(name) = &e_expr.kind {
                    if name == "end" {
                        None // Treat 'end' as an omitted end bound.
                    } else {
                        // It's a regular variable/expression, analyze normally.
                        let typed_e = self.analyze_expression(e_expr, ctx, Some(&JophetType::UInt(64)), errors);
                        if !matches!(typed_e.jophet_type, JophetType::Int(_) | JophetType::UInt(_)) {
                            errors.push(SemanticError::TypeError {
                                message: "Slice end index must be an integer.".to_string(),
                                span: e_expr.span.clone(),
                                file_path: self.current_module_path.clone(),
                            });
                        }
                        Some(Box::new(typed_e))
                    }
                } else {
                    let typed_e = self.analyze_expression(e_expr, ctx, Some(&JophetType::UInt(64)), errors);
                    if !matches!(typed_e.jophet_type, JophetType::Int(_) | JophetType::UInt(_)) {
                        errors.push(SemanticError::TypeError {
                            message: "Slice end index must be an integer.".to_string(),
                            span: e_expr.span.clone(),
                            file_path: self.current_module_path.clone(),
                        });
                    }
                    Some(Box::new(typed_e))
                }
            }
            None => None,
        };

        if let JophetType::PythonObject { .. } = typed_array.jophet_type {
            self.needs_python_runtime = true;

            // Create a call to the built-in `slice(start, end)` function.
            let mut slice_args = Vec::new();
            slice_args.push(typed_start.clone().map_or_else(
                || TypedExpression {
                    kind: TypedExpressionKind::Literal(Literal::Nothing),
                    jophet_type: JophetType::Nothing,
                    span: span.clone(),
                },
                |s| *s,
            ));
            slice_args.push(typed_end.clone().map_or_else(
                || TypedExpression {
                    kind: TypedExpressionKind::Literal(Literal::Nothing),
                    jophet_type: JophetType::Nothing,
                    span: span.clone(),
                },
                |e| *e,
            ));

            let slice_object_expr = TypedExpression {
                kind: TypedExpressionKind::FunctionCall {
                    kind: TypedCallKind::Named("slice".to_string()),
                    args: slice_args,
                },
                // The type of this intermediate expression is a special marker for the backend.
                jophet_type: JophetType::PythonSlice,
                span: span.clone(),
            };

            // Now, create the `__getitem__` call.
            return Ok(TypedExpression {
                kind: TypedExpressionKind::MethodCall {
                    object: Box::new(typed_array),
                    mangled_name: "__getitem__".to_string(),
                    args: vec![slice_object_expr],
                },
                jophet_type: JophetType::PythonObject { brand: Box::new(self.py_any_brand.clone()) },
                span,
            });
        }
        
        let (member_type, size) = match &typed_array.jophet_type {
            JophetType::Array { member_type, size } => (member_type.as_ref().clone(), Some(*size)),
            JophetType::Vector(member_type) => (member_type.as_ref().clone(), None),
            JophetType::String | JophetType::StringSlice => (JophetType::Char, None),
            _ => {
                return Err(SemanticError::TypeError {
                    message: format!(
                        "Cannot slice a value of type '{}'. Only Array, Vector, String, and PythonObject types are sliceable.",
                        jophet_type_to_user_string(&typed_array.jophet_type)
                    ),
                    span: array.span.clone(),
                    file_path: self.current_module_path.clone(),
                });
            }
        };

        let result_type = match &typed_array.jophet_type {
            JophetType::String | JophetType::StringSlice => JophetType::String,
            _ => JophetType::Vector(Box::new(member_type)),
        };

        Ok(TypedExpression {
            kind: TypedExpressionKind::ArraySlice {
                array: Box::new(typed_array),
                start: typed_start,
                end: typed_end,
                size,
            },
            jophet_type: result_type,
            span,
        })
    }
}