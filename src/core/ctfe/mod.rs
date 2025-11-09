// src/core/ctfe/mod.rs
//! The Compile-Time Function Execution (CTFE) engine.
//!
//! This module contains the interpreter responsible for executing a subset of the
//! Jophet language at compile time. It is invoked by the semantic analyzer when
//! a `const` function call is encountered. The interpreter operates on the
//! `TypedProgram` AST and produces `ComptimeValue`s, which are then transformed
//! back into literal AST nodes, effectively replacing the function call with its result.
//!
//! The interpreter is sandboxed and will produce an error if it encounters any
//! operation with runtime side effects (e.g., I/O, FFI).

use crate::core::ast::typed::{JophetType, TypedExpression, TypedFunctionDecl};
use std::collections::HashMap;

pub mod interpreter;

/// Represents a value that can exist and be manipulated at compile time.
/// This is the primary data structure used by the CTFE interpreter. It now
/// includes variants for all standard language constructs, including tuples,
/// structs, arrays, and vectors, to enable full compile-time evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum ComptimeValue {
    Int(i64),
    UInt(u64),
    Float(f64),
    Bool(bool),
    Char(char),
    String(String),
    Tuple(Vec<ComptimeValue>),
    /// A struct value, holding the struct's type name and a map of its field values.
    Struct(String, HashMap<String, ComptimeValue>),
    /// An array value, represented as a vector of compile-time values.
    Array(Vec<ComptimeValue>),
    /// A vector value, also represented as a vector of compile-time values.
    /// At compile time, there is no distinction in representation between Array and Vector.
    Vector(Vec<ComptimeValue>),
    /// Represents the `Nothing` value at compile time.
    Nothing,
}

/// A variable in the compile-time execution context.
#[derive(Debug, Clone)]
pub struct ComptimeVar {
    pub value: ComptimeValue,
    pub is_mutable: bool,
}

impl std::fmt::Display for ComptimeValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComptimeValue::Int(v) => write!(f, "{}", v),
            ComptimeValue::UInt(v) => write!(f, "{}", v),
            ComptimeValue::Float(v) => write!(f, "{}", v),
            ComptimeValue::Bool(v) => write!(f, "{}", v),
            ComptimeValue::Char(v) => write!(f, "'{}'", v),
            ComptimeValue::String(v) => write!(f, "\"{}\"", v),
            ComptimeValue::Nothing => write!(f, "nothing"),
            ComptimeValue::Tuple(elements) => {
                let inner = elements
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "({})", inner)
            }
            ComptimeValue::Struct(name, fields) => {
                let inner = fields
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{}({})", name, inner)
            }
            ComptimeValue::Array(elements) | ComptimeValue::Vector(elements) => {
                let inner = elements
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "[{}]", inner)
            }
        }
    }
}

/// An error that occurs during compile-time function execution.
#[derive(Debug, Clone)]
pub enum CtfeError {
    /// The operation is not supported in a compile-time context.
    UnsupportedOperation(String),
    /// An attempt was made to call a function with side effects (e.g., `println`).
    ImpureFunctionCall(String),
    /// A required function definition was not found.
    FunctionNotFound(String),
    /// A variable was used that was not a compile-time constant.
    NonConstantValue(String),
    /// An arithmetic error occurred, such as division by zero.
    ArithmeticError(String),
    /// A dependency required for evaluation is not yet available and needs to be computed.
    DependencyNotReady(String),
    /// A control flow error occurred, such as a switch not yielding a value.
    FlowError(String),
    /// A type mismatch occurred during compile-time evaluation.
    TypeError {
        expected: String,
        found: String,
    },
}

impl std::fmt::Display for CtfeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CtfeError::UnsupportedOperation(op) => write!(f, "unsupported operation: {}", op),
            CtfeError::ImpureFunctionCall(name) => {
                write!(f, "call to impure function '{}' is not allowed", name)
            }
            CtfeError::FunctionNotFound(name) => write!(f, "could not find function '{}'", name),
            CtfeError::NonConstantValue(name) => {
                write!(f, "value of '{}' is not known at compile time", name)
            }
            CtfeError::ArithmeticError(msg) => write!(f, "arithmetic error: {}", msg),
            CtfeError::DependencyNotReady(name) => write!(f, "dependency '{}' has not been evaluated yet", name),
            CtfeError::FlowError(msg) => write!(f, "control flow error: {}", msg),
            CtfeError::TypeError { expected, found } => {
                write!(f, "type mismatch: expected {}, found {}", expected, found)
            }
        }
    }
}