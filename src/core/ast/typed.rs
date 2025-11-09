// src/core/ast/typed.rs
//! Defines the Typed Abstract Syntax Tree (AST).
//!
//! This module contains the data structures for the AST *after* semantic analysis
//! and type checking have been completed. Every expression node (`TypedExpression`)
//! is annotated with its resolved `JophetType`. Untyped constructs from the parsing
//! stage are replaced with their semantically validated and fully-typed counterparts.
//! Doc comments from the untyped AST are carried over to the typed declaration nodes.
//!
//! A call to a function prefixed with `const` will be evaluated at compile time and
//! replaced with a literal in the final AST.

use crate::core::ast::common::{Literal, Span, TokenKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// An enum representing all possible types in the Jophet language, fully resolved.
///
/// This is a central data structure in the compiler, used throughout the semantic
/// analysis and code generation phases. It is serializable to support the creation
/// of library metadata files.
#[derive(Debug, Clone, Eq, Serialize, Deserialize)]
pub enum JophetType {
    /// A special sentinel type used internally by the semantic analyzer to represent
    /// an expression that failed to type-check. This prevents error cascades.
    ErrorSentinel,

    // Primitive Types
    Int(u8),
    UInt(u8),
    Float(u8),
    Bool,
    Char,
    String,
    StringSlice,
    /// The `Nothing` type, used to represent the absence of a value, typically for
    /// functions that do not return anything.
    Nothing,
    /// An unsigned integer whose size is the same as a pointer on the target architecture.
    /// Maps to C's `size_t`.
    USize,
    /// A signed integer whose size is the same as a pointer on the target architecture.
    /// Maps to C's `ptrdiff_t`.
    ISize,

    /// Represents an imported module.
    Module {
        name: String,
    },

    // User-Defined Aggregate Types
    Struct {
        name: String,
        /// The absolute path to the module file where this struct is defined.
        module_path: PathBuf,
    },
    Enum {
        name: String,
        /// A vector of (name, value, doc_comment) tuples for each fully-resolved enum member.
        members: Vec<(String, i64, Option<String>)>,
        /// The absolute path to the module file where this enum is defined.
        module_path: PathBuf,
    },
    Union {
        name: String,
        /// The absolute path to the module file where this union is defined.
        module_path: PathBuf,
    },
    TaggedUnion {
        name: String,
        /// The absolute path to the module file where this tagged union is defined.
        module_path: PathBuf,
    },
    Error {
        name: String,
        /// The absolute path to the module file where this error type is defined.
        module_path: PathBuf,
    },
    /// A universal error type used by all fallible (`?`) functions. It can represent
    /// any user-defined `error` or a built-in error (like a string).
    AnyError,
    Trait {
        name: String,
        /// The absolute path to the module file where this trait is defined.
        module_path: PathBuf,
    },

    // Pointer and Reference Types
    Pointer(Box<JophetType>),
    Reference(Box<JophetType>),
    MutableReference(Box<JophetType>),
    RawPointer(Box<JophetType>),

    // Collection Types
    Tuple(Vec<JophetType>),
    Array {
        member_type: Box<JophetType>,
        size: usize,
    },
    Vector(Box<JophetType>),
    Dictionary {
        key: Box<JophetType>,
        value: Box<JophetType>,
    },

    /// A temporary, internal-only type representing an array of a known
    /// element type but an unknown size. This is created from an `Array<T>`
    /// annotation and must be resolved to a sized `JophetType::Array` by
    /// an initializer.
    UnsizedArray(Box<JophetType>),

    /// Represents a generic type parameter within a generic context, e.g., `T`.
    GenericParam {
        name: String,
    },

    // Other Types
    Function {
        params: Vec<JophetType>,
        ret: Box<JophetType>,
    },
    /// A closure type, representing a callable object.
    Closure {
        params: Vec<JophetType>,
        ret: Box<JophetType>,
        /// The mangled name of the underlying C function that implements this closure.
        mangled_name: String,
        /// The name of the C struct used for this closure's environment.
        env_struct_name: String,
    },
    /// A fallible type, representing a value that can be either a success (`ok`)
    /// or a failure (`err`). Analogous to `Result<T, E>`.
    Fallible {
        ok: Box<JophetType>,
        err: Box<JophetType>,
    },
    /// A handle to an included C header file for FFI.
    CLibrary {
        header: PathBuf,
    },
    /// A handle to an imported Python module for FFI.
    PythonModule,
    /// An opaque handle to a generic Python object returned from an FFI call.
    /// This is a generic type that holds a "brand" (a marker type like `PyList` or `PyInt`)
    /// to provide static type information about the foreign object.
    PythonObject {
        brand: Box<JophetType>,
    },
    /// A temporary, internal-only type representing a Python slice object.
    /// This is created by the `slice()` built-in and consumed by the `__getitem__`
    /// magic method analysis. It has no direct C representation.
    PythonSlice,
}

// Custom PartialEq implementation to correctly compare named types like structs
// by name only, without considering their fields or module paths for type equality checks.
impl PartialEq for JophetType {
    fn eq(&self, other: &Self) -> bool {
        // First, check if the enum variants are the same.
        std::mem::discriminant(self) == std::mem::discriminant(other)
            && match (self, other) {
                // For primitives with sizes, compare the sizes.
                (JophetType::Int(s), JophetType::Int(o)) => s == o,
                (JophetType::UInt(s), JophetType::UInt(o)) => s == o,
                (JophetType::Float(s), JophetType::Float(o)) => s == o,
                // For named types, compare by name.
                (JophetType::Module { name: s_name }, JophetType::Module { name: o_name }) => s_name == o_name,
                (
                    JophetType::Struct {
                        name: s_name, ..
                    },
                    JophetType::Struct {
                        name: o_name, ..
                    },
                ) => s_name == o_name,
                (
                    JophetType::Enum {
                        name: s_name, ..
                    },
                    JophetType::Enum {
                        name: o_name, ..
                    },
                ) => s_name == o_name,
                (
                    JophetType::Union {
                        name: s_name, ..
                    },
                    JophetType::Union {
                        name: o_name, ..
                    },
                ) => s_name == o_name,
                (
                    JophetType::TaggedUnion {
                        name: s_name, ..
                    },
                    JophetType::TaggedUnion {
                        name: o_name, ..
                    },
                ) => s_name == o_name,
                (
                    JophetType::Error {
                        name: s_name, ..
                    },
                    JophetType::Error {
                        name: o_name, ..
                    },
                ) => s_name == o_name,
                (
                    JophetType::Trait {
                        name: s_name, ..
                    },
                    JophetType::Trait {
                        name: o_name, ..
                    },
                ) => s_name == o_name,
                (JophetType::GenericParam { name: s_name }, JophetType::GenericParam { name: o_name }) => s_name == o_name,
                // For compound types, recurse.
                (JophetType::Pointer(s), JophetType::Pointer(o)) => s == o,
                (JophetType::Reference(s), JophetType::Reference(o)) => s == o,
                (JophetType::MutableReference(s), JophetType::MutableReference(o)) => s == o,
                (JophetType::RawPointer(s), JophetType::RawPointer(o)) => s == o,
                (JophetType::Tuple(s), JophetType::Tuple(o)) => s == o,
                (JophetType::Array { member_type: s, size: s_size }, JophetType::Array { member_type: o, size: o_size }) => s == o && s_size == o_size,
                (JophetType::Vector(s), JophetType::Vector(o)) => s == o,
                (JophetType::Dictionary { key: sk, value: sv }, JophetType::Dictionary { key: ok, value: ov }) => sk == ok && sv == ov,
                (JophetType::UnsizedArray(s), JophetType::UnsizedArray(o)) => s == o,
                (JophetType::Function { params: sp, ret: sr }, JophetType::Function { params: op, ret: or }) => sp == op && sr == or,
                (JophetType::Closure { params: sp, ret: sr, .. }, JophetType::Closure { params: op, ret: or, .. }) => sp == op && sr == or,
                (JophetType::Fallible { ok: so, err: se }, JophetType::Fallible { ok: oo, err: oe }) => so == oo && se == oe,
                (JophetType::PythonObject { brand: s_brand }, JophetType::PythonObject { brand: o_brand }) => s_brand == o_brand,
                // For simple, parameterless types, the discriminant check is sufficient.
                (JophetType::CLibrary { .. }, JophetType::CLibrary { .. }) => true,
                (JophetType::Bool, JophetType::Bool)
                | (JophetType::Char, JophetType::Char)
                | (JophetType::String, JophetType::String)
                | (JophetType::StringSlice, JophetType::StringSlice)
                | (JophetType::Nothing, JophetType::Nothing)
                | (JophetType::USize, JophetType::USize)
                | (JophetType::ISize, JophetType::ISize)
                | (JophetType::AnyError, JophetType::AnyError)
                | (JophetType::ErrorSentinel, JophetType::ErrorSentinel)
                | (JophetType::PythonModule, JophetType::PythonModule)
                | (JophetType::PythonSlice, JophetType::PythonSlice) => true,
                _ => false,
            }
    }
}

// Custom Hash implementation that mirrors the logic of PartialEq.
impl Hash for JophetType {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            JophetType::Int(s) => s.hash(state),
            JophetType::UInt(s) => s.hash(state),
            JophetType::Float(s) => s.hash(state),
            JophetType::Module { name } => name.hash(state),
            JophetType::Struct { name, .. } => name.hash(state),
            JophetType::Enum { name, .. } => name.hash(state),
            JophetType::Union { name, .. } => name.hash(state),
            JophetType::TaggedUnion { name, .. } => name.hash(state),
            JophetType::Error { name, .. } => name.hash(state),
            JophetType::Trait { name, .. } => name.hash(state),
            JophetType::GenericParam { name } => name.hash(state),
            JophetType::Pointer(s) => s.hash(state),
            JophetType::Reference(s) => s.hash(state),
            JophetType::MutableReference(s) => s.hash(state),
            JophetType::RawPointer(s) => s.hash(state),
            JophetType::Tuple(s) => s.hash(state),
            JophetType::Array { member_type, size } => {
                member_type.hash(state);
                size.hash(state);
            }
            JophetType::Vector(s) => s.hash(state),
            JophetType::Dictionary { key, value } => {
                key.hash(state);
                value.hash(state);
            }
            JophetType::UnsizedArray(s) => s.hash(state),
            JophetType::Function { params, ret } => {
                params.hash(state);
                ret.hash(state);
            }
            JophetType::Closure { params, ret, .. } => {
                params.hash(state);
                ret.hash(state);
            }
            JophetType::Fallible { ok, err } => {
                ok.hash(state);
                err.hash(state);
            }
            JophetType::PythonObject { brand } => brand.hash(state),
            JophetType::CLibrary { .. } => {
                // We ignore the header for hashing to match PartialEq
            }
            _ => {}
        }
    }
}

/// Represents a fully typed generic parameter with its resolved trait bounds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedGenericParam {
    pub name: String,
    pub bounds: Vec<JophetType>,
}

/// Represents the public signature of a method, for serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicMethodInfo {
    pub name: String,
    pub mangled_name: String,
    pub params: Vec<(String, JophetType)>, // Includes 'self'
    pub return_type: JophetType,
}

/// Represents a part of a typed interpolated string.
#[derive(Debug, Clone)]
pub enum TypedInterpolationPart {
    /// A literal string segment.
    Literal(String),
    /// A fully typed and analyzed expression to be evaluated and formatted.
    Expression(TypedExpression),
}

/// A node in the Typed AST representing an expression, annotated with its type.
#[derive(Debug, Clone)]
pub struct TypedExpression {
    pub kind: TypedExpressionKind,
    pub jophet_type: JophetType,
    pub span: Span,
}

/// Represents a pattern in a typed `switch` case.
#[derive(Debug, Clone)]
pub enum TypedPattern {
    /// A fully-typed literal pattern.
    Literal(TypedExpression),
    /// A fully-typed and validated destructuring pattern.
    Destructure {
        /// The fully resolved type of the enum being matched (e.g., `Option<String>`).
        enum_type: JophetType,
        variant_name: String,
        /// The variable binding, now including its resolved type.
        binding: Option<(String, JophetType)>,
    },
}

/// Represents a variable captured by a closure from its environment.
#[derive(Debug, Clone)]
pub struct TypedCapturedVariable {
    /// The name of the captured variable in the outer scope.
    pub name: String,
    /// The type of the captured variable.
    pub jophet_type: JophetType,
    /// `true` if the captured variable is mutable.
    pub is_mutable: bool,
}

/// Represents the different kinds of callable entities.
#[derive(Debug, Clone)]
pub enum TypedCallKind {
    /// A call to a named function.
    Named(String),
    /// A call to a closure variable.
    Closure {
        /// The expression representing the closure variable itself.
        callable_expr: Box<TypedExpression>,
        /// The expected parameter types of the closure.
        params: Vec<JophetType>,
        /// The expected return type of the closure.
        ret: Box<JophetType>,
    },
}

/// An enum representing all possible kinds of expressions in the Typed AST.
#[derive(Debug, Clone)]
pub enum TypedExpressionKind {
    /// A special sentinel kind used internally by the semantic analyzer to represent
    /// an expression that failed to type-check. This prevents error cascades.
    Error,
    /// A `new` expression for creating instances of certain types (e.g., `new String`).
    New {
        jophet_type: JophetType,
        args: Vec<TypedExpression>,
    },
    Literal(Literal),
    /// A literal `UInt64` value that does not fit in an `i64`. This is produced
    /// by compile-time evaluation and is distinct from the general `Literal` enum
    /// to handle large unsigned integers correctly.
    UInt64Literal(u64),
    /// An identifier. If it refers to an imported or global symbol, `mangled_name` will be `Some`.
    Identifier {
        name: String,
        mangled_name: Option<String>,
    },
    BinaryOp(Box<TypedExpression>, TokenKind, Box<TypedExpression>),
    UnaryOp(TokenKind, Box<TypedExpression>),
    TernaryOp(Box<TypedExpression>, Box<TypedExpression>, Box<TypedExpression>),
    /// An anonymous function that creates a closure value.
    Closure {
        /// The underlying function implementing the closure's logic.
        function: TypedFunctionDecl,
        /// A list of variables captured from the environment.
        captures: Vec<TypedCapturedVariable>,
    },
    /// A call to a function or a closure to be evaluated at runtime.
    FunctionCall {
        kind: TypedCallKind,
        args: Vec<TypedExpression>,
    },
    /// A call to a function to be evaluated at compile time. This node exists only
    /// ephemerally during semantic analysis and is replaced by a `Literal` node.
    ConstCall {
        kind: TypedCallKind,
        args: Vec<TypedExpression>,
    },
    InterpolatedString(Vec<TypedInterpolationPart>),
    /// A struct instantiation, e.g., `MyStruct(id: 1, name: "a")`.
    StructInstantiation(String, Vec<(String, TypedExpression)>),
    /// A union instantiation, e.g., `MyUnion(f: 3.14)`.
    UnionInstantiation {
        union_name: String,
        field_name: String,
        value: Box<TypedExpression>,
    },
    /// A tagged union instantiation with a payload, eg., `MyEnum.Variant(42)`.
    TaggedUnionInstantiation {
        enum_name: String,
        variant_name: String,
        payload: Option<Box<TypedExpression>>,
    },
    /// An access to an enum variant without instantiation, e.g., `MyEnum.Variant`.
    /// This is strictly for C-style enums.
    EnumVariantAccess {
        enum_name: String,
        variant_name: String,
    },
    FieldAccess(Box<TypedExpression>, String),
    /// A call to a method on an object. The `mangled_name` can be a simple name for
    /// built-in methods (like `length`) which the backend handles specially, or a
    /// fully mangled name for user-defined methods. Some methods like `.map()` may
    /// require the backend to generate a block of statements rather than a simple expression.
    MethodCall {
        object: Box<TypedExpression>,
        mangled_name: String,
        args: Vec<TypedExpression>,
    },
    Tuple(Vec<TypedExpression>),
    TupleAccess(Box<TypedExpression>, usize),
    AddressOf(Box<TypedExpression>),
    Dereference(Box<TypedExpression>),
    ArrayLiteral(Vec<TypedExpression>),
    ArrayIndex {
        array: Box<TypedExpression>,
        index: Box<TypedExpression>,
        /// The compile-time size of the array, if known. Used for bounds checking.
        size: Option<usize>,
    },
    ArraySlice {
        array: Box<TypedExpression>,
        start: Option<Box<TypedExpression>>,
        end: Option<Box<TypedExpression>>,
        /// The compile-time size of the original array, if known.
        size: Option<usize>,
    },
    DictionaryInstantiation {
        key_type: JophetType,
        value_type: JophetType,
        pairs: Vec<(TypedExpression, TypedExpression)>,
    },
    Switch {
        expression: Box<TypedExpression>,
        cases: Vec<TypedSwitchCase>,
        else_block: Option<Vec<TypedStatement>>,
    },
    /// A `try` expression that propagates an error from the current function.
    PropagateError {
        expr: Box<TypedExpression>,
    },
    /// A `try` expression in a non-fallible context that unwraps a value or panics on error.
    UnwrapOrPanic {
        expr: Box<TypedExpression>,
    },
    /// A `catch` expression for handling errors from a fallible expression.
    /// The AST node is target-agnostic; the backend is responsible for generating
    /// the correct code for the specific `Result` type.
    Catch {
        expression: Box<TypedExpression>,
        error_variable: String,
        body: Vec<TypedStatement>,
    },
    /// An implicit conversion of a value into a `Fallible` type.
    /// This node is inserted by the semantic analyzer.
    FallibleWrap {
        is_ok: bool,
        expr: Box<TypedExpression>,
    },
    /// An implicit conversion of a specific error type (e.g., `Fallible<T, String>`)
    /// into a general one (`Fallible<T, AnyError>`).
    ErrorUpcast {
        expr: Box<TypedExpression>,
    },
    /// An `allow` expression, which signifies a block where unsafe operations
    /// are permitted.
    Allow(Box<TypedExpression>),
    /// An explicit type conversion, e.g., `convert(my_int)`.
    Convert {
        expr: Box<TypedExpression>,
        target_type: JophetType,
    },
    /// An explicit `clone` operation to create a deep copy of an owned value.
    Clone(Box<TypedExpression>),
    /// A call to the built-in `parse(Type, String)` function.
    Parse {
        target_type: JophetType,
        expr: Box<TypedExpression>,
    },
    /// A call to the built-in `includeC(header)` function.
    IncludeC {
        header: String,
    },
    /// A call to the built-in `importPy(module)` function.
    ImportPy {
        module_name: String,
    },
}

impl TypedExpressionKind {
    /// Helper to identify expressions that are lvalues (i.e., they can appear on the left side of an assignment).
    pub fn is_lvalue(&self) -> bool {
        matches!(self,
            TypedExpressionKind::Identifier { .. }
            | TypedExpressionKind::FieldAccess(_, _)
            | TypedExpressionKind::ArrayIndex { .. }
            | TypedExpressionKind::TupleAccess(_, _)
            | TypedExpressionKind::Dereference(_)
        )
    }
}

/// A node in the Typed AST representing a statement.
#[derive(Debug, Clone)]
pub struct TypedStatement {
    pub kind: TypedStatementKind,
    pub span: Span,
}

/// A fully typed struct definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedStructDef {
    pub is_public: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    pub generic_params: Vec<TypedGenericParam>,
    /// A vector of (name, type, is_public) tuples for each field.
    pub fields: Vec<(String, JophetType, bool)>,
    pub module_path: PathBuf,
}

/// A fully typed enum definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedEnumDef {
    pub is_public: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    /// A vector of (name, value, doc_comment) tuples for each fully-resolved enum member.
    pub members: Vec<(String, i64, Option<String>)>,
    pub module_path: PathBuf,
}

/// A fully typed union definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedUnionDef {
    pub is_public: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    /// A vector of (name, type, doc_comment) tuples for each field.
    pub fields: Vec<(String, JophetType, Option<String>)>,
    pub module_path: PathBuf,
}

/// A variant of a fully typed tagged union or error type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedTaggedUnionVariant {
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    /// The type of the payload, if any.
    pub payload: Option<JophetType>,
}

/// A fully typed tagged union definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedTaggedUnionDef {
    pub is_public: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    pub generic_params: Vec<TypedGenericParam>,
    pub variants: Vec<TypedTaggedUnionVariant>,
    pub module_path: PathBuf,
}

/// A fully typed error definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedErrorDef {
    pub is_public: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    pub variants: Vec<TypedTaggedUnionVariant>,
    pub module_path: PathBuf,
}

/// A fully typed trait definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedTraitDef {
    pub is_public: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    pub generic_params: Vec<TypedGenericParam>,
    pub methods: Vec<TypedFunctionDecl>,
    pub module_path: PathBuf,
}

/// Represents the `else` or `else if` part of a typed `if` statement.
#[derive(Debug, Clone)]
pub enum TypedElseBlock {
    ElseIf(TypedIfStatement),
    Else(Vec<TypedStatement>),
}

/// A fully typed `if` statement.
#[derive(Debug, Clone)]
pub struct TypedIfStatement {
    /// For a simple `if`, this is the boolean condition. For `if let`, it's the fallible expression.
    pub condition: TypedExpression,
    /// If this is an `if let` binding, this holds the variable's name and its fully resolved type.
    pub binding: Option<(String, JophetType)>,
    pub then_block: Vec<TypedStatement>,
    pub else_block: Option<Box<TypedElseBlock>>,
}

/// A fully typed `while` statement.
#[derive(Debug, Clone)]
pub struct TypedWhileStatement {
    pub condition: TypedExpression,
    pub body: Vec<TypedStatement>,
}

/// A fully typed numeric range-based `for` statement.
#[derive(Debug, Clone)]
pub struct TypedForStatement {
    pub iterator_name: String,
    pub iterator_type: JophetType,
    pub start: TypedExpression,
    pub stop: TypedExpression,
    pub step: Option<TypedExpression>,
    pub body: Vec<TypedStatement>,
}

/// A fully typed iterable-based `for-in` statement.
#[derive(Debug, Clone)]
pub struct TypedForInStatement {
    pub iterator_name: String,
    pub iterator_type: JophetType,
    pub collection: TypedExpression,
    pub body: Vec<TypedStatement>,
}

/// A fully typed variable declaration.
#[derive(Debug, Clone)]
pub struct TypedVariableDecl {
    pub name: String,
    /// Whether this variable was declared with `const` (compile-time evaluated).
    pub is_const: bool,
    pub is_mutable: bool,
    pub jophet_type: JophetType,
    pub initializer: TypedExpression,
}

/// A single target in a fully typed destructuring declaration.
/// Note: The `..` rest pattern and `_` discard targets are filtered out during semantic analysis
/// and do not create `TypedDestructuringTarget` instances.
#[derive(Debug, Clone)]
pub struct TypedDestructuringTarget {
    /// The new variable name being declared.
    pub var_name: String,
    /// The resolved type of the variable.
    pub jophet_type: JophetType,
    /// Whether the new variable is declared as `mutable`.
    pub is_mutable: bool,
    /// The name of the field from the source struct, if this is a named destructuring.
    /// If `None`, this is a positional destructuring (for tuples or structs).
    pub source_field: Option<String>,
}

/// A fully typed destructuring declaration for a tuple or struct.
#[derive(Debug, Clone)]
pub struct DestructuringDecl {
    /// A vector of targets, one for each new variable.
    pub targets: Vec<TypedDestructuringTarget>,
    pub initializer: TypedExpression,
}

/// A fully typed destructuring declaration for an array.
#[derive(Debug, Clone)]
pub struct ArrayDestructuringDecl {
    /// A vector of targets, one for each new variable.
    pub targets: Vec<TypedDestructuringTarget>,
    pub initializer: TypedExpression,
}

/// The left-hand side of a typed assignment, which can be a simple expression, a tuple, or an array.
#[derive(Debug, Clone)]
pub enum TypedAssignmentLValue {
    /// A standard l-value expression, e.g., `x`, `s.field`.
    Expression(TypedExpression),
    /// A tuple of l-value expressions for destructuring assignment, e.g., `(x, s.field)`.
    /// Note: The `..` rest pattern is not allowed in destructuring assignment.
    Tuple(Vec<TypedExpression>),
    /// An array of l-value expressions for destructuring assignment, e.g., `[x, y]`.
    Array(Vec<TypedExpression>),
}

/// A fully typed function declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypedFunctionDecl {
    pub is_public: bool,
    #[serde(default)]
    pub is_const: bool,
    pub name: String,
    #[serde(default)]
    pub doc_comment: Option<String>,
    pub generic_params: Vec<TypedGenericParam>,
    /// The globally unique, mangled name for the function used by the backend.
    pub mangled_name: String,
    pub params: Vec<(String, JophetType)>,
    pub return_type: JophetType,
    #[serde(skip)]
    pub body: Vec<TypedStatement>,
    /// If this is a method, this is the name of the struct it belongs to.
    pub receiver_type: Option<String>,
    /// If this is a closure, this holds the variables it captures.
    #[serde(skip)]
    pub captures: Option<Vec<TypedCapturedVariable>>,
}

/// A case within a typed `switch` expression, containing one or more patterns.
#[derive(Debug, Clone)]
pub struct TypedSwitchCase {
    pub patterns: Vec<TypedPattern>,
    pub body: Vec<TypedStatement>,
}

/// An enum representing all possible kinds of statements in the Typed AST.
#[derive(Debug, Clone)]
pub enum TypedStatementKind {
    VariableDecl(TypedVariableDecl),
    DestructuringDecl(DestructuringDecl),
    ArrayDestructuringDecl(ArrayDestructuringDecl),
    FunctionDecl(TypedFunctionDecl),
    If(TypedIfStatement),
    While(TypedWhileStatement),
    For(TypedForStatement),
    ForIn(TypedForInStatement),
    StructDef(TypedStructDef),
    EnumDef(TypedEnumDef),
    UnionDef(TypedUnionDef),
    TaggedUnionDef(TypedTaggedUnionDef),
    ErrorDef(TypedErrorDef),
    TraitDef(TypedTraitDef),
    Break,
    Continue,
    ExpressionStatement(TypedExpression),
    Return(TypedExpression),
    Assignment(TypedAssignmentLValue, TypedExpression),
    /// A manual, immediate `delete` statement.
    Delete(String, JophetType),
    /// An automatic `delete` statement inserted by the compiler at the end of a scope.
    AutoDelete(String, JophetType),
    /// A `yield` statement from a switch expression.
    Yield(TypedExpression),
}

/// The root of the Typed AST, representing a complete, analyzed program.
pub type TypedProgram = Vec<TypedStatement>;