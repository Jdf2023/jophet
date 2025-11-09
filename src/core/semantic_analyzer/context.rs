// src/core/semantic_analyzer/context.rs
//! Defines the context and scope tracking structures for semantic analysis.
//!
//! This module contains the core data structures used by the semantic analyzer to
//! keep track of symbols, types, ownership, and borrow states within different scopes.
//! `ScopeContext` is the primary structure that gets passed around during the analysis
//! of a block of code.

use crate::core::ast::typed::{
    JophetType, PublicMethodInfo, TypedEnumDef, TypedErrorDef, TypedStructDef,
    TypedTaggedUnionDef, TypedTraitDef, TypedUnionDef, TypedFunctionDecl,
};
use crate::core::ast::untyped;
use crate::core::ctfe::ComptimeValue;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Information about a symbol (variable, function, etc.) in a scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInfo {
    /// The resolved `JophetType` of the symbol.
    pub jophet_type: JophetType,
    /// `true` if the symbol represents a mutable variable.
    pub is_mutable: bool,
    /// `true` if the symbol represents a compile-time const variable.
    #[serde(default)]
    pub is_const: bool,
    /// The globally unique mangled name for a function, if applicable.
    pub mangled_name: Option<String>,
}

/// Represents the public API of an imported module.
///
/// This structure is serialized to a `.jophet-meta` file when a library is installed.
/// It allows the semantic analyzer to understand the types and functions available
/// from a pre-compiled library without needing its source code.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleScope {
    /// Public functions available in the module.
    pub symbol_table: HashMap<String, SymbolInfo>,
    /// Public struct definitions.
    pub struct_defs: HashMap<String, TypedStructDef>,
    /// Public enum definitions.
    pub enum_defs: HashMap<String, TypedEnumDef>,
    /// Public union definitions.
    pub union_defs: HashMap<String, TypedUnionDef>,
    /// Public tagged union definitions.
    pub tagged_union_defs: HashMap<String, TypedTaggedUnionDef>,
    /// Public error definitions.
    pub error_defs: HashMap<String, TypedErrorDef>,
    /// Public trait definitions.
    pub trait_defs: HashMap<String, TypedTraitDef>,
    /// Public method definitions, keyed by struct name, then method name.
    pub method_defs: HashMap<String, HashMap<String, PublicMethodInfo>>,
    /// Public function definitions with their bodies, keyed by mangled name.
    /// This is needed for compile-time function execution of imported functions.
    /// Note: This field is populated during compilation but not serialized (function bodies are skipped).
    #[serde(skip)]
    pub function_defs: HashMap<String, TypedFunctionDecl>,
}

/// Represents the current borrowing state of an owned variable.
/// This is a key part of the simplified borrow checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BorrowState {
    /// The variable is not currently borrowed.
    Unique,
    /// The variable is immutably borrowed one or more times.
    Borrowed { immutable_count: usize },
    /// The variable is mutably borrowed.
    MutablelyBorrowed,
}

/// A type alias for a unique identifier for a heap allocation.
pub type AllocationId = usize;

/// The main context for semantic analysis within a specific lexical scope.
///
/// An instance of `ScopeContext` is created for each function body or block. It
/// tracks all local variables, their types, ownership, and borrow status.
#[derive(Debug, Clone)]
pub struct ScopeContext {
    /// Maps variable names to their type and mutability information.
    pub symbol_table: HashMap<String, SymbolInfo>,
    /// Tracks the current borrow state of each owned variable.
    pub borrow_states: HashMap<String, BorrowState>,
    /// Maps a borrow variable to the name of the variable it borrows from.
    pub borrows: HashMap<String, String>,
    /// A set of variable names whose ownership has been moved.
    pub moved_vars: HashSet<String>,
    /// A set of variable names whose resources have been explicitly deleted.
    pub deleted_vars: HashSet<String>,
    /// Maps an owning variable name to its unique allocation ID.
    pub ownership_map: HashMap<String, AllocationId>,
    /// The inferred yield type for the current `switch` expression, if any.
    pub current_switch_yield_type: Option<JophetType>,
    /// The expected type of the variable currently being declared, for inference.
    pub current_variable_decl_type: Option<Box<JophetType>>,
    /// The return type of the current function, if inside one. Used to validate `try` and `return`.
    pub current_function_return_type: Option<JophetType>,
    /// Tracks active generic parameters and their trait bounds. This is populated
    /// when analyzing the body of a generic function.
    pub generic_context: HashMap<String, Vec<JophetType>>,
    /// A map of generic parameter substitutions to apply when resolving types.
    /// This is populated during monomorphization.
    pub substitutions: HashMap<String, JophetType>,
    /// A flag indicating if the current expression is being analyzed inside an `allow` block.
    pub in_allow_block: bool,
    /// A flag indicating if the current expression is being analyzed in a compile-time context.
    pub in_const_context: bool,
    /// Stores the compile-time values of variables that have been evaluated.
    pub comptime_values: HashMap<String, ComptimeValue>,
    /// A way to find the declaration AST node for a variable, needed for recursive const evaluation.
    pub declaration_map: HashMap<String, untyped::VariableDecl>,
    /// A counter to generate unique allocation IDs.
    next_alloc_id: AllocationId,
}

impl ScopeContext {
    /// Creates a new, empty `ScopeContext`.
    pub fn new() -> Self {
        ScopeContext {
            symbol_table: HashMap::new(),
            borrow_states: HashMap::new(),
            borrows: HashMap::new(),
            moved_vars: HashSet::new(),
            deleted_vars: HashSet::new(),
            ownership_map: HashMap::new(),
            current_switch_yield_type: None,
            current_variable_decl_type: None,
            current_function_return_type: None,
            generic_context: HashMap::new(),
            substitutions: HashMap::new(),
            in_allow_block: false,
            in_const_context: false,
            comptime_values: HashMap::new(),
            declaration_map: HashMap::new(),
            next_alloc_id: 0,
        }
    }

    /// Generates a new, unique ID for a heap allocation.
    pub fn new_alloc_id(&mut self) -> AllocationId {
        let id = self.next_alloc_id;
        self.next_alloc_id += 1;
        id
    }

    /// Updates the borrow state of a variable when a borrow's lifetime ends.
    pub fn release_borrow(&mut self, owner_name: &str) {
        let new_state = match self.borrow_states.get_mut(owner_name) {
            Some(BorrowState::Borrowed { immutable_count }) => {
                *immutable_count -= 1;
                // If this was the last immutable borrow, the state becomes unique again.
                if *immutable_count == 0 {
                    Some(BorrowState::Unique)
                } else {
                    None
                }
            }
            // A mutable borrow is exclusive, so releasing it makes the state unique.
            Some(BorrowState::MutablelyBorrowed) => Some(BorrowState::Unique),
            _ => None,
        };
        if let Some(state) = new_state {
            self.borrow_states.insert(owner_name.to_string(), state);
        }
    }
}