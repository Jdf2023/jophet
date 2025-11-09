// src/core/ctfe/interpreter.rs
//! The interpreter for Compile-Time Function Execution (CTFE).
//!
//! This module provides the logic for evaluating `TypedExpression` nodes at
//! compile time. It operates in a sandboxed environment, disallowing any
//! operations with side effects. It now supports local variables (mutable and immutable),
//! assignments, `if/else`, `while`, and `for` loop control flow, `switch` expressions,
//! and recursive `const` calls via memoization. It can also trigger the recursive,
//! demand-driven evaluation of dependencies by calling back into the semantic analyzer.
//! During arithmetic operations, it will panic on overflow.

use super::{ComptimeValue, ComptimeVar, CtfeError};
use crate::core::ast::typed::{
    JophetType, TypedAssignmentLValue, TypedCallKind, TypedElseBlock, TypedExpression,
    TypedExpressionKind, TypedPattern, TypedStatement, TypedStatementKind, TypedFunctionDecl,
};
use crate::core::ast::untyped::DeclarationPattern;
use crate::core::ast::Literal;
use crate::core::ast::TokenKind;
use crate::core::semantic_analyzer::{ScopeContext, SemanticAnalyzer};
use crate::diagnostics::errors::SemanticError;
use std::cell::RefCell;
use std::collections::HashMap;

/// A safeguard to prevent the compiler from hanging on infinite loops at compile time.
const CTFE_ITERATION_LIMIT: u32 = 1_000_000;

/// The result of executing a statement at compile time.
enum StatementResult {
    /// The statement executed normally, and control flow should continue to the next statement.
    Normal,
    /// A `return` statement was executed, and the function should exit with the given value.
    Return(ComptimeValue),
    /// A `yield` statement was executed from a `switch` expression block.
    Yield(ComptimeValue),
    /// A `break` statement was executed, and the current loop should terminate.
    Break,
    /// A `continue` statement was executed, and the current loop should proceed to the next iteration.
    Continue,
}

/// The CTFE interpreter. It owns the function definitions and a shared cache
/// to memoize results of `const` calls.
pub struct Interpreter<'a, 'ctx> {
    monomorphized_functions: HashMap<String, TypedFunctionDecl>,
    imported_functions: HashMap<String, TypedFunctionDecl>,
    /// The cache for memoizing `const` function call results.
    /// The key is a unique string representing the call (e.g., "fib(10)").
    cache: &'a RefCell<HashMap<String, ComptimeValue>>,
    
    /// A mutable reference back to the `SemanticAnalyzer`. This is essential for the
    /// demand-driven evaluation model, as it allows the interpreter to request that the
    /// analyzer compute the value of a dependency that is not yet known at compile-time.
    analyzer: &'a mut SemanticAnalyzer<'ctx>,
}

impl<'a, 'ctx> Interpreter<'a, 'ctx> {
    /// Creates a new interpreter instance with access to function definitions, a shared cache,
    /// and a reference to the parent semantic analyzer for callbacks.
    pub fn new(
        monomorphized_functions: HashMap<String, TypedFunctionDecl>,
        imported_functions: HashMap<String, TypedFunctionDecl>,
        cache: &'a RefCell<HashMap<String, ComptimeValue>>,
        analyzer: &'a mut SemanticAnalyzer<'ctx>, // Add analyzer reference
    ) -> Self {
        Self {
            monomorphized_functions,
            imported_functions,
            cache,
            analyzer,
        }
    }

    /// The main entry point for evaluating a `const` function call.
    ///
    /// It sets up a new evaluation context for the function call, populates it with the
    /// arguments, and then executes the statements in the function's body one by one.
    /// It correctly handles `return` statements and implicit returns for `Nothing` functions.
    /// It now uses memoization to handle recursive calls efficiently and prevent hangs.
    pub fn evaluate_function_call(
        &mut self,
        mangled_name: &str,
        args: &[TypedExpression],
        caller_ctfe_ctx: &mut HashMap<String, ComptimeVar>,
        analyzer_ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<ComptimeValue, CtfeError> {
        // Evaluate arguments in the *caller's* context first. This is crucial for recursion.
        let arg_values: Vec<ComptimeValue> = args
            .iter()
            .map(|arg| self.evaluate_expression(arg, caller_ctfe_ctx, analyzer_ctx, errors))
            .collect::<Result<_, _>>()?;

        // --- MEMOIZATION LOGIC ---
        // Create a unique key for this specific call (function name + argument values).
        let cache_key = format!(
            "{}({})",
            mangled_name,
            arg_values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );

        // Check if the result is already in the cache.
        if let Some(cached_value) = self.cache.borrow().get(&cache_key) {
            return Ok(cached_value.clone());
        }
        // --- END MEMOIZATION LOGIC ---

        // Find the function definition.
        let func_decl = if let Some(decl) = self.monomorphized_functions.get(mangled_name) {
            decl.clone()
        } else if let Some(decl) = self.imported_functions.get(mangled_name) {
            decl.clone()
        } else {
            return Err(CtfeError::FunctionNotFound(mangled_name.to_string()));
        };

        // A `const` function must not have captures.
        if func_decl.captures.as_ref().map_or(false, |c| !c.is_empty()) {
            return Err(CtfeError::UnsupportedOperation(
                "const evaluation of closures with captures is not supported".to_string(),
            ));
        }

        // Create a new, fresh context for this function call.
        let mut call_ctx = HashMap::new();
        for ((param_name, _), arg_val) in func_decl.params.iter().zip(arg_values) {
            // Function parameters are never mutable within the function body.
            call_ctx.insert(
                param_name.clone(),
                ComptimeVar {
                    value: arg_val,
                    is_mutable: false,
                },
            );
        }

        // Execute the function body statement by statement.
        let mut result_value = ComptimeValue::Nothing; // Default for Nothing-returning functions
        for stmt in &func_decl.body {
            match self.evaluate_statement(stmt, &mut call_ctx, analyzer_ctx, errors)? {
                StatementResult::Normal => continue, // Continue to the next statement
                StatementResult::Return(value) => {
                    result_value = value;
                    break; // Exit the loop on return
                }
                StatementResult::Break | StatementResult::Continue => {
                    return Err(CtfeError::FlowError(
                        "`break` or `continue` outside of a loop.".to_string(),
                    ));
                }
                StatementResult::Yield(_) => {
                    return Err(CtfeError::FlowError(
                        "`yield` outside of a switch expression.".to_string(),
                    ));
                }
            }
        }

        // Final check for functions that must return a value.
        if func_decl.return_type != JophetType::Nothing && result_value == ComptimeValue::Nothing {
            if !func_decl
                .body
                .iter()
                .any(|s| matches!(s.kind, TypedStatementKind::Return(_)))
            {
                return Err(CtfeError::UnsupportedOperation(
                    "A `const` function with a non-Nothing return type must end with an explicit `return` statement."
                        .to_string(),
                ));
            }
        }

        // Store the computed result in the cache before returning.
        self.cache
            .borrow_mut()
            .insert(cache_key, result_value.clone());

        Ok(result_value)
    }

    /// Evaluates a single statement within a `const` context.
    ///
    /// This function handles `return`, variable declarations, assignments (to simple variables
    /// and array elements), and all control flow statements (`if/else`, `while`, `for`).
    /// It modifies the evaluation context (`ctx`) for variable declarations and returns a
    /// `StatementResult` to signal control flow changes. For loops, it modifies the context
    /// in-place to allow mutations to persist across iterations.
    fn evaluate_statement(
        &mut self,
        stmt: &TypedStatement,
        ctfe_ctx: &mut HashMap<String, ComptimeVar>,
        analyzer_ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<StatementResult, CtfeError> {
        match &stmt.kind {
            TypedStatementKind::VariableDecl(decl) => {
                // Allow `let`, `mutable`, and `const` declarations.
                let value = self.evaluate_expression(&decl.initializer, ctfe_ctx, analyzer_ctx, errors)?;
                ctfe_ctx.insert(
                    decl.name.clone(),
                    ComptimeVar {
                        value,
                        is_mutable: decl.is_mutable,
                    },
                );
                Ok(StatementResult::Normal)
            }

            TypedStatementKind::Assignment(lvalue, rvalue) => {
                // First, evaluate the right-hand side to get the new value.
                let new_value = self.evaluate_expression(rvalue, ctfe_ctx, analyzer_ctx, errors)?;

                // Now, handle the different kinds of l-values.
                match lvalue {
                    TypedAssignmentLValue::Expression(lvalue_expr) => match &lvalue_expr.kind {
                        TypedExpressionKind::Identifier { name, .. } => {
                            let var = ctfe_ctx.get_mut(name).ok_or_else(|| {
                                CtfeError::NonConstantValue("Assignment to unknown variable".to_string())
                            })?;

                            if !var.is_mutable {
                                return Err(CtfeError::UnsupportedOperation(format!(
                                    "Cannot assign to immutable variable `{}` in a const context.",
                                    name
                                )));
                            }

                            var.value = new_value;
                            Ok(StatementResult::Normal)
                        }

                        TypedExpressionKind::ArrayIndex { array, index, .. } => {
                            // Find out which variable the array is.
                            let array_var_name = if let TypedExpressionKind::Identifier { name, .. } = &array.kind {
                                name
                            } else {
                                return Err(CtfeError::UnsupportedOperation(
                                    "Compile-time assignment is only supported for arrays that are simple variables.".to_string(),
                                ));
                            };
                            
                            // Evaluate the index expression.
                            let index_val = self.evaluate_expression(index, ctfe_ctx, analyzer_ctx, errors)?;
                            let idx = if let ComptimeValue::Int(i) = index_val {
                                i as usize
                            } else {
                                return Err(CtfeError::TypeError {
                                    expected: "Integer".to_string(),
                                    found: "a non-integer index for array assignment".to_string(),
                                });
                            };

                            // Get the mutable variable from the context.
                            let var = ctfe_ctx.get_mut(array_var_name).ok_or_else(|| {
                                CtfeError::NonConstantValue(format!("Assignment to unknown array '{}'", array_var_name))
                            })?;

                            if !var.is_mutable {
                                return Err(CtfeError::UnsupportedOperation(format!(
                                    "Cannot assign to element of immutable array `{}` in a const context.",
                                    array_var_name
                                )));
                            }

                            // Update the array value.
                            if let ComptimeValue::Array(elements) = &mut var.value {
                                if idx >= elements.len() {
                                    return Err(CtfeError::ArithmeticError(format!(
                                        "Index {} out of bounds for array of length {}",
                                        idx,
                                        elements.len()
                                    )));
                                }
                                elements[idx] = new_value;
                                Ok(StatementResult::Normal)
                            } else {
                                Err(CtfeError::TypeError {
                                    expected: "Array".to_string(),
                                    found: "a non-array type for indexed assignment".to_string(),
                                })
                            }
                        }
                        
                        _ => Err(CtfeError::UnsupportedOperation(
                            "Only assignment to variables and array elements is supported in `const` functions."
                                .to_string(),
                        )),
                    },
                    _ => Err(CtfeError::UnsupportedOperation(
                        "Destructuring assignment is not yet supported in `const` functions."
                            .to_string(),
                    )),
                }
            }

            TypedStatementKind::Return(ret_expr) => {
                let value = self.evaluate_expression(ret_expr, ctfe_ctx, analyzer_ctx, errors)?;
                Ok(StatementResult::Return(value))
            }
            
            TypedStatementKind::Yield(yield_expr) => {
                let value = self.evaluate_expression(yield_expr, ctfe_ctx, analyzer_ctx, errors)?;
                Ok(StatementResult::Yield(value))
            }
            
            TypedStatementKind::Break => Ok(StatementResult::Break),
            TypedStatementKind::Continue => Ok(StatementResult::Continue),

            TypedStatementKind::If(if_stmt) => {
                let condition_val = self.evaluate_expression(&if_stmt.condition, ctfe_ctx, analyzer_ctx, errors)?;
                let condition_bool = if let ComptimeValue::Bool(b) = condition_val {
                    b
                } else {
                    return Err(CtfeError::TypeError {
                        expected: "Bool".to_string(),
                        found: "a non-boolean condition for an if statement".to_string(),
                    });
                };

                if condition_bool {
                    // Execute the `then` block.
                    for inner_stmt in &if_stmt.then_block {
                        let result = self.evaluate_statement(inner_stmt, ctfe_ctx, analyzer_ctx, errors)?;
                        if !matches!(result, StatementResult::Normal) {
                             return Ok(result);
                        }
                    }
                } else if let Some(else_block) = &if_stmt.else_block {
                    // Execute the `else` or `else if` block.
                    match else_block.as_ref() {
                        TypedElseBlock::Else(stmts) => {
                            for inner_stmt in stmts {
                                let result = self.evaluate_statement(inner_stmt, ctfe_ctx, analyzer_ctx, errors)?;
                                if !matches!(result, StatementResult::Normal) {
                                     return Ok(result);
                                }
                            }
                        }
                        TypedElseBlock::ElseIf(next_if) => {
                            // Recursively evaluate the `else if`.
                            let temp_stmt = TypedStatement {
                                kind: TypedStatementKind::If(next_if.clone()),
                                span: next_if.condition.span.clone(),
                            };
                            return self.evaluate_statement(&temp_stmt, ctfe_ctx, analyzer_ctx, errors);
                        }
                    }
                }

                Ok(StatementResult::Normal)
            }

            TypedStatementKind::While(while_stmt) => {
                let mut iteration_count = 0;
                loop {
                    iteration_count += 1;
                    if iteration_count > CTFE_ITERATION_LIMIT {
                        return Err(CtfeError::UnsupportedOperation(format!(
                            "Compile-time evaluation exceeded the maximum loop iteration limit of {}. This is a safeguard against potential infinite loops.",
                            CTFE_ITERATION_LIMIT
                        )));
                    }

                    let condition_val = self.evaluate_expression(&while_stmt.condition, ctfe_ctx, analyzer_ctx, errors)?;
                    let condition_bool = if let ComptimeValue::Bool(b) = condition_val {
                        b
                    } else {
                        return Err(CtfeError::TypeError {
                            expected: "Bool".to_string(),
                            found: "a non-boolean condition for a while loop".to_string(),
                        });
                    };

                    if !condition_bool {
                        break; // Exit the loop
                    }

                    // Execute the loop body
                    for inner_stmt in &while_stmt.body {
                        match self.evaluate_statement(inner_stmt, ctfe_ctx, analyzer_ctx, errors)? {
                            StatementResult::Normal => continue,
                            StatementResult::Return(val) => return Ok(StatementResult::Return(val)),
                            StatementResult::Break => return Ok(StatementResult::Normal), // Stop loop execution
                            StatementResult::Continue => break, // Go to next loop iteration
                            StatementResult::Yield(_) => return Err(CtfeError::UnsupportedOperation("`yield` is not allowed inside a `while` loop".to_string())),
                        }
                    }
                }
                Ok(StatementResult::Normal)
            }
            
            TypedStatementKind::For(for_stmt) => {
                let start_val = self.evaluate_expression(&for_stmt.start, ctfe_ctx, analyzer_ctx, errors)?;
                let stop_val = self.evaluate_expression(&for_stmt.stop, ctfe_ctx, analyzer_ctx, errors)?;
                let step_val = if let Some(step_expr) = &for_stmt.step {
                    self.evaluate_expression(step_expr, ctfe_ctx, analyzer_ctx, errors)?
                } else {
                    ComptimeValue::Int(1)
                };

                let (mut i, stop, step) = match (start_val, stop_val, step_val) {
                    (ComptimeValue::Int(s), ComptimeValue::Int(o), ComptimeValue::Int(t)) => (s, o, t),
                    _ => return Err(CtfeError::TypeError {
                        expected: "Integer".to_string(),
                        found: "non-integer loop bounds".to_string(),
                    }),
                };

                if step == 0 {
                    return Err(CtfeError::ArithmeticError("For loop step cannot be zero".to_string()));
                }

                let mut iteration_count = 0;
                let condition = |i: i64| if step > 0 { i <= stop } else { i >= stop };

                'outer: while condition(i) {
                    iteration_count += 1;
                    if iteration_count > CTFE_ITERATION_LIMIT {
                        return Err(CtfeError::UnsupportedOperation(format!(
                            "Compile-time evaluation exceeded the maximum loop iteration limit of {}.",
                            CTFE_ITERATION_LIMIT
                        )));
                    }

                    ctfe_ctx.insert(for_stmt.iterator_name.clone(), ComptimeVar {
                        value: ComptimeValue::Int(i),
                        is_mutable: false,
                    });

                    for inner_stmt in &for_stmt.body {
                        match self.evaluate_statement(inner_stmt, ctfe_ctx, analyzer_ctx, errors)? {
                            StatementResult::Normal => continue,
                            StatementResult::Return(val) => {
                                ctfe_ctx.remove(&for_stmt.iterator_name); // Cleanup before returning
                                return Ok(StatementResult::Return(val));
                            }
                            StatementResult::Break => {
                                ctfe_ctx.remove(&for_stmt.iterator_name); // Cleanup before breaking
                                break 'outer;
                            }
                            StatementResult::Continue => break,
                            StatementResult::Yield(_) => return Err(CtfeError::UnsupportedOperation("`yield` is not allowed in a `for` loop".to_string())),
                        }
                    }
                    ctfe_ctx.remove(&for_stmt.iterator_name);
                    i += step;
                }
                Ok(StatementResult::Normal)
            }

            TypedStatementKind::ForIn(for_in_stmt) => {
                let collection_val = self.evaluate_expression(&for_in_stmt.collection, ctfe_ctx, analyzer_ctx, errors)?;

                let iterable: Vec<ComptimeValue> = match collection_val {
                    ComptimeValue::Array(elements) => elements,
                    ComptimeValue::Vector(elements) => elements,
                    ComptimeValue::String(s) => s.chars().map(ComptimeValue::Char).collect(),
                    _ => return Err(CtfeError::TypeError {
                        expected: "an iterable (Array, Vector, String)".to_string(),
                        found: "a non-iterable type".to_string(),
                    }),
                };

                'outer: for item in iterable {
                    ctfe_ctx.insert(for_in_stmt.iterator_name.clone(), ComptimeVar {
                        value: item,
                        is_mutable: false,
                    });

                    for inner_stmt in &for_in_stmt.body {
                        match self.evaluate_statement(inner_stmt, ctfe_ctx, analyzer_ctx, errors)? {
                            StatementResult::Normal => continue,
                            StatementResult::Return(val) => {
                                ctfe_ctx.remove(&for_in_stmt.iterator_name); // Cleanup
                                return Ok(StatementResult::Return(val));
                            }
                            StatementResult::Break => {
                                ctfe_ctx.remove(&for_in_stmt.iterator_name); // Cleanup
                                break 'outer;
                            }
                            StatementResult::Continue => break, // Breaks from inner loop to next outer iteration
                            StatementResult::Yield(_) => return Err(CtfeError::UnsupportedOperation("`yield` is not allowed in a `for` loop".to_string())),
                        }
                    }
                    ctfe_ctx.remove(&for_in_stmt.iterator_name);
                }
                Ok(StatementResult::Normal)
            }


            _ => Err(CtfeError::UnsupportedOperation(format!(
                "Statement kind `{:?}` is not yet supported in `const` functions.",
                stmt.kind
            ))),
        }
    }

    /// Evaluates a `TypedExpression` node and returns a `ComptimeValue`. This function now
    /// supports a wide range of constructs, including struct instantiations, field access,
    /// array literals, array indexing, `switch` expressions, and recursive `const` calls.
    /// It is now type-aware when evaluating literals.
    /// `ctfe_ctx` stores the values of local variables for the current evaluation frame.
    /// `analyzer` is the parent semantic analyzer instance, used for recursive evaluation.
    ///
    /// When this function encounters an identifier whose compile-time value is not yet known,
    /// it now directly invokes the `SemanticAnalyzer` (via its mutable reference) to analyze
    /// and compute the dependency's value on the spot. This makes the interpreter the sole
    /// driver of the demand-driven evaluation process.
    pub fn evaluate_expression(
        &mut self,
        expr: &TypedExpression,
        ctfe_ctx: &mut HashMap<String, ComptimeVar>,
        analyzer_ctx: &mut ScopeContext,
        errors: &mut Vec<SemanticError>,
    ) -> Result<ComptimeValue, CtfeError> {
        match &expr.kind {
            TypedExpressionKind::Literal(lit) => {
                match (&lit, &expr.jophet_type) {
                    (Literal::Int(i), JophetType::UInt(_)) => Ok(ComptimeValue::UInt(*i as u64)),
                    (Literal::Int(i), _) => Ok(ComptimeValue::Int(*i)),
                    (Literal::Float(f), _) => Ok(ComptimeValue::Float(*f)),
                    (Literal::String(s), _) => Ok(ComptimeValue::String(s.clone())),
                    (Literal::Char(c), _) => Ok(ComptimeValue::Char(*c)),
                    (Literal::Bool(b), _) => Ok(ComptimeValue::Bool(*b)),
                    (Literal::Nothing, _) => Ok(ComptimeValue::Nothing),
                }
            }
            TypedExpressionKind::Identifier { name, .. } => {
                // 1. Check local CTFE context (function arguments/locals).
                if let Some(var) = ctfe_ctx.get(name) {
                    return Ok(var.value.clone());
                }

                // 2. Check the global analyzer context for an already-computed value.
                if let Some(value) = analyzer_ctx.comptime_values.get(name) {
                    return Ok(value.clone());
                }
                
                // --- START OF FIX ---
                // 3. If the value is not found, it's a dependency that needs to be evaluated now.
                // Find the untyped declaration for the dependency.
                let decl_node = if let Some(decl) = analyzer_ctx.declaration_map.get(name).cloned() {
                    decl
                } else {
                    // If the declaration doesn't even exist, it's not a dependency, it's a true name error.
                    return Err(CtfeError::NonConstantValue(format!("Could not find declaration for '{}'", name)));
                };

                // For now, only handle simple variable declarations as dependencies.
                if let DeclarationPattern::Identifier(decl_name, decl_type) = &decl_node.pattern {
                    // Call the analyzer to evaluate this dependency NOW.
                    // This is a recursive call, but it's controlled.
                    let _ = self.analyzer.analyze_simple_variable_decl(
                        decl_name,
                        decl_type,
                        &decl_node,
                        decl_node.is_mutable,
                        decl_node.is_const,
                        true, // CRUCIAL: We are demanding a compile-time value.
                        analyzer_ctx, // Pass the MUTABLE context
                        decl_node.initializer.span.clone(),
                        errors,
                    );

                    // After the call, the value SHOULD be in the context. Let's look for it again.
                    if let Some(value) = analyzer_ctx.comptime_values.get(name) {
                        return Ok(value.clone());
                    } else {
                        // If it's still not there, it means the dependency was not compile-time computable.
                        return Err(CtfeError::NonConstantValue(format!("Failed to resolve compile-time dependency '{}'", name)));
                    }
                } else {
                    return Err(CtfeError::UnsupportedOperation("Dependency resolution for destructuring is not yet supported.".to_string()));
                }
                // --- END OF FIX ---
            }
            TypedExpressionKind::TernaryOp(cond, then, else_b) => {
                let cond_val = self.evaluate_expression(cond, ctfe_ctx, analyzer_ctx, errors)?;
                if let ComptimeValue::Bool(b) = cond_val {
                    if b {
                        self.evaluate_expression(then, ctfe_ctx, analyzer_ctx, errors)
                    } else {
                        self.evaluate_expression(else_b, ctfe_ctx, analyzer_ctx, errors)
                    }
                } else {
                    Err(CtfeError::TypeError {
                        expected: "Bool".to_string(),
                        found: "a non-boolean condition for ternary operator".to_string(),
                    })
                }
            }
            TypedExpressionKind::BinaryOp(left, op, right) => {
                let left_val = self.evaluate_expression(left, ctfe_ctx, analyzer_ctx, errors)?;
                let right_val = self.evaluate_expression(right, ctfe_ctx, analyzer_ctx, errors)?;
                self.evaluate_binary_op(&left_val, op, &right_val)
            }
            TypedExpressionKind::FunctionCall { kind, args } => {
                if let TypedCallKind::Named(mangled_name) = kind {
                    if self.is_impure_builtin(mangled_name) {
                        return Err(CtfeError::ImpureFunctionCall(mangled_name.clone()));
                    }
                    self.evaluate_function_call(mangled_name, args, ctfe_ctx, analyzer_ctx, errors)
                } else {
                    Err(CtfeError::UnsupportedOperation(
                        "Closure calls are not supported at compile time.".to_string(),
                    ))
                }
            }
            TypedExpressionKind::ConstCall { kind, args } => {
                if let TypedCallKind::Named(mangled_name) = kind {
                    self.evaluate_function_call(mangled_name, args, ctfe_ctx, analyzer_ctx, errors)
                } else {
                    Err(CtfeError::UnsupportedOperation(
                        "Compile-time evaluation of closures is not supported.".to_string(),
                    ))
                }
            }
            TypedExpressionKind::StructInstantiation(name, args) => {
                let mut fields = HashMap::new();
                for (field_name, field_expr) in args {
                    let field_value = self.evaluate_expression(field_expr, ctfe_ctx, analyzer_ctx, errors)?;
                    fields.insert(field_name.clone(), field_value);
                }
                Ok(ComptimeValue::Struct(name.clone(), fields))
            }
            TypedExpressionKind::FieldAccess(object, field_name) => {
                let object_value = self.evaluate_expression(object, ctfe_ctx, analyzer_ctx, errors)?;
                if let ComptimeValue::Struct(_, fields) = object_value {
                    fields.get(field_name)
                        .cloned()
                        .ok_or_else(|| CtfeError::NonConstantValue(format!("Field '{}' not found on compile-time struct", field_name)))
                } else {
                    Err(CtfeError::TypeError {
                        expected: "a struct".to_string(),
                        found: "a non-struct type for field access".to_string(),
                    })
                }
            }
            TypedExpressionKind::ArrayLiteral(elements) => {
                let mut const_elements = Vec::new();
                for el in elements {
                    const_elements.push(self.evaluate_expression(el, ctfe_ctx, analyzer_ctx, errors)?);
                }
                Ok(ComptimeValue::Array(const_elements))
            }
            TypedExpressionKind::ArrayIndex { array, index, .. } => {
                let array_val = self.evaluate_expression(array, ctfe_ctx, analyzer_ctx, errors)?;
                let index_val = self.evaluate_expression(index, ctfe_ctx, analyzer_ctx, errors)?;

                if let ComptimeValue::Array(elements) = array_val {
                    if let ComptimeValue::Int(i) = index_val {
                        elements.get(i as usize)
                            .cloned()
                            .ok_or_else(|| CtfeError::ArithmeticError(format!("Index {} out of bounds for array of length {}", i, elements.len())))
                    } else {
                        Err(CtfeError::TypeError { expected: "Integer".to_string(), found: "a non-integer index".to_string() })
                    }
                } else {
                    Err(CtfeError::TypeError { expected: "Array".to_string(), found: "a non-array type for indexing".to_string() })
                }
            }
            TypedExpressionKind::Switch { expression, cases, else_block } => {
                let switch_on_value = self.evaluate_expression(expression, ctfe_ctx, analyzer_ctx, errors)?;
                let mut yielded_value = None;
            
                'case_loop: for case in cases {
                    for pattern in &case.patterns {
                        if let TypedPattern::Literal(lit_expr) = pattern {
                            let pattern_value = self.evaluate_expression(lit_expr, ctfe_ctx, analyzer_ctx, errors)?;
                            if switch_on_value == pattern_value {
                                for case_stmt in &case.body {
                                    match self.evaluate_statement(case_stmt, ctfe_ctx, analyzer_ctx, errors)? {
                                        StatementResult::Yield(val) => {
                                            yielded_value = Some(val);
                                            break 'case_loop;
                                        },
                                        StatementResult::Normal => continue,
                                        _ => return Err(CtfeError::UnsupportedOperation("`return`, `break`, or `continue` is not allowed inside a `switch` expression.".to_string())),
                                    }
                                }
                            }
                        } else {
                             return Err(CtfeError::UnsupportedOperation("Destructuring patterns in `const switch` are not yet supported.".to_string()));
                        }
                    }
                }
                
                if yielded_value.is_none() {
                    if let Some(else_stmts) = else_block {
                         for else_stmt in else_stmts {
                            match self.evaluate_statement(else_stmt, ctfe_ctx, analyzer_ctx, errors)? {
                                StatementResult::Yield(val) => {
                                    yielded_value = Some(val);
                                    break;
                                },
                                StatementResult::Normal => continue,
                                _ => return Err(CtfeError::UnsupportedOperation("`return`, `break`, or `continue` is not allowed inside a `switch` expression.".to_string())),
                            }
                        }
                    }
                }
                
                yielded_value.ok_or_else(|| CtfeError::FlowError("Switch expression did not yield a value".to_string()))
            }
            _ => Err(CtfeError::UnsupportedOperation(format!(
                "Expression kind `{:?}` is not yet supported at compile time.",
                expr.kind
            ))),
        }
    }

    /// Evaluates a binary operation on two compile-time values, checking for overflow.
    fn evaluate_binary_op(
        &self,
        left: &ComptimeValue,
        op: &TokenKind,
        right: &ComptimeValue,
    ) -> Result<ComptimeValue, CtfeError> {
        match (left, right) {
            (ComptimeValue::Int(l), ComptimeValue::Int(r)) => match op {
                TokenKind::Plus => l.checked_add(*r).map(ComptimeValue::Int).ok_or_else(|| CtfeError::ArithmeticError("Compile-time integer addition overflowed".to_string())),
                TokenKind::Minus => l.checked_sub(*r).map(ComptimeValue::Int).ok_or_else(|| CtfeError::ArithmeticError("Compile-time integer subtraction overflowed".to_string())),
                TokenKind::Asterisk => l.checked_mul(*r).map(ComptimeValue::Int).ok_or_else(|| CtfeError::ArithmeticError("Compile-time integer multiplication overflowed".to_string())),
                TokenKind::Slash => l.checked_div(*r).map(ComptimeValue::Int).ok_or_else(|| CtfeError::ArithmeticError("Compile-time division by zero or overflow".to_string())),
                TokenKind::Percent => l.checked_rem(*r).map(ComptimeValue::Int).ok_or_else(|| CtfeError::ArithmeticError("Compile-time modulo by zero or overflow".to_string())),
                TokenKind::EqualEqual => Ok(ComptimeValue::Bool(l == r)),
                TokenKind::BangEquals => Ok(ComptimeValue::Bool(l != r)),
                TokenKind::LAngle => Ok(ComptimeValue::Bool(l < r)),
                TokenKind::RAngle => Ok(ComptimeValue::Bool(l > r)),
                TokenKind::LessEquals => Ok(ComptimeValue::Bool(l <= r)),
                TokenKind::GreaterEquals => Ok(ComptimeValue::Bool(l >= r)),
                _ => Err(CtfeError::UnsupportedOperation(format!(
                    "Operator `{:?}` on integers",
                    op
                ))),
            },
            (ComptimeValue::UInt(l), ComptimeValue::UInt(r)) => match op {
                TokenKind::Plus => l.checked_add(*r).map(ComptimeValue::UInt).ok_or_else(|| CtfeError::ArithmeticError("Compile-time unsigned integer addition overflowed".to_string())),
                TokenKind::Minus => l.checked_sub(*r).map(ComptimeValue::UInt).ok_or_else(|| CtfeError::ArithmeticError("Compile-time unsigned integer subtraction overflowed".to_string())),
                TokenKind::Asterisk => l.checked_mul(*r).map(ComptimeValue::UInt).ok_or_else(|| CtfeError::ArithmeticError("Compile-time unsigned integer multiplication overflowed".to_string())),
                TokenKind::Slash => l.checked_div(*r).map(ComptimeValue::UInt).ok_or_else(|| CtfeError::ArithmeticError("Compile-time unsigned division by zero".to_string())),
                TokenKind::Percent => l.checked_rem(*r).map(ComptimeValue::UInt).ok_or_else(|| CtfeError::ArithmeticError("Compile-time unsigned modulo by zero".to_string())),
                TokenKind::EqualEqual => Ok(ComptimeValue::Bool(l == r)),
                TokenKind::BangEquals => Ok(ComptimeValue::Bool(l != r)),
                TokenKind::LAngle => Ok(ComptimeValue::Bool(l < r)),
                TokenKind::RAngle => Ok(ComptimeValue::Bool(l > r)),
                TokenKind::LessEquals => Ok(ComptimeValue::Bool(l <= r)),
                TokenKind::GreaterEquals => Ok(ComptimeValue::Bool(l >= r)),
                _ => Err(CtfeError::UnsupportedOperation(format!(
                    "Operator `{:?}` on unsigned integers",
                    op
                ))),
            },
            (ComptimeValue::Float(l), ComptimeValue::Float(r)) => match op {
                TokenKind::Plus => Ok(ComptimeValue::Float(l + r)),
                TokenKind::Minus => Ok(ComptimeValue::Float(l - r)),
                TokenKind::Asterisk => Ok(ComptimeValue::Float(l * r)),
                TokenKind::Slash => Ok(ComptimeValue::Float(l / r)),
                TokenKind::EqualEqual => Ok(ComptimeValue::Bool(l == r)),
                TokenKind::BangEquals => Ok(ComptimeValue::Bool(l != r)),
                TokenKind::LAngle => Ok(ComptimeValue::Bool(l < r)),
                TokenKind::RAngle => Ok(ComptimeValue::Bool(l > r)),
                TokenKind::LessEquals => Ok(ComptimeValue::Bool(l <= r)),
                TokenKind::GreaterEquals => Ok(ComptimeValue::Bool(l >= r)),
                _ => Err(CtfeError::UnsupportedOperation(format!(
                    "Operator `{:?}` on floats",
                    op
                ))),
            },
            (ComptimeValue::Bool(l), ComptimeValue::Bool(r)) => match op {
                TokenKind::AmpersandAmpersand => Ok(ComptimeValue::Bool(*l && *r)),
                TokenKind::PipePipe => Ok(ComptimeValue::Bool(*l || *r)),
                TokenKind::EqualEqual => Ok(ComptimeValue::Bool(l == r)),
                TokenKind::BangEquals => Ok(ComptimeValue::Bool(l != r)),
                _ => Err(CtfeError::UnsupportedOperation(format!(
                    "Operator `{:?}` on booleans",
                    op
                ))),
            },
            _ => Err(CtfeError::TypeError {
                expected: "operands of the same primitive type".to_string(),
                found: "mixed or non-primitive types".to_string(),
            }),
        }
    }

    /// Checks if a built-in function is impure and cannot be run at compile time.
    fn is_impure_builtin(&self, name: &str) -> bool {
        matches!(
            name,
            "println"
                | "print"
                | "input"
                | "command"
                | "read"
                | "write"
                | "readLines"
                | "writeLines"
                | "jophet_allocate"
                | "jophet_deallocate"
                | "importPy"
        )
    }
}