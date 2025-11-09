// src/core/ast/untyped.rs
//! Defines the Untyped Abstract Syntax Tree (AST).
//!
//! This module contains the data structures that represent the program's structure
//! directly after parsing. At this stage, the AST is "untyped" because type checking
//! and semantic validation have not yet occurred. Type annotations are represented
//! as simple strings or unresolved structures (`untyped::Type`). This AST is the
//! direct output of the parser and the input to the semantic analyzer. Doc comments
//! are captured and stored on declaration nodes.
//!
//! The `PythonObject` type annotation can be used with generic arguments to specify
//! a "brand", like `PythonObject<PyList>`, providing hints to the semantic analyzer.
//!
//! Variable and function declarations now carry an `is_const` flag when prefixed
//! by the `const` keyword. A `const` keyword can also prefix a function call to
//! request compile-time evaluation.

pub use crate::core::ast::common::{Literal, Span, TokenKind};
use std::fmt;
use std::path::PathBuf;

/// Represents a type annotation as seen by the parser, before semantic analysis.
#[derive(Debug, Clone, PartialEq)]
pub enum Type {
    /// A simple type name, e.g., `Int64` or `MyStruct`.
    Simple(String),
    /// A generic type, e.g., `Vector<String>`.
    Generic(String, Vec<Type>),
    /// A fixed-size array type, e.g., `Array<Int64, 10>`.
    Array(Box<Type>, i64),
    /// An immutable reference, e.g., `&MyStruct`.
    Reference(Box<Type>),
    /// A mutable reference, e.g., `&mutable MyStruct`.
    MutableReference(Box<Type>),
    /// A raw, unsafe pointer, e.g., `raw *Int64`.
    RawPointer(Box<Type>),
    /// A fallible type, e.g., `MyType?`.
    Fallible(Box<Type>),
    /// A closure type, e.g., `Closure<(Int64): String>`.
    Closure {
        params: Vec<Type>,
        ret: Box<Type>,
    },
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Simple(name) => write!(f, "{}", name),
            Type::Generic(name, params) => {
                let param_str = params
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{}<{}>", name, param_str)
            }
            Type::Array(inner, size) => write!(f, "Array<{}, {}>", inner, size),
            Type::Reference(inner) => write!(f, "&{}", inner),
            Type::MutableReference(inner) => write!(f, "&mutable {}", inner),
            Type::RawPointer(inner) => write!(f, "raw *{}", inner),
            Type::Fallible(inner) => write!(f, "{}?", inner),
            Type::Closure { params, ret } => {
                let param_str = params
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "Closure<({}): {}>", param_str, ret)
            }
        }
    }
}

/// Represents a single generic parameter with its trait bounds.
#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub bounds: Vec<Type>,
}

/// Represents a part of an untyped interpolated string.
#[derive(Debug, Clone)]
pub enum InterpolationPart {
    /// A literal string segment.
    Literal(String),
    /// An unevaluated expression to be formatted.
    Expression(Expression),
}

/// Represents an argument in a `new` or struct/union instantiation expression.
/// An argument can be positional (`my_func(10)`), named (`my_struct(x: 10)`), or a
/// key-value pair (`new Dictionary("key": value)`).
#[derive(Debug, Clone)]
pub enum Arg {
    /// A positional argument, like `10`.
    Positional(Expression),
    /// A named argument for a struct field, like `x: 10`.
    Named(String, Expression),
    /// A key-value pair for a dictionary initializer, like `"key": 10`.
    KeyValuePair(Expression, Expression),
}

/// A node in the Untyped AST representing an expression.
#[derive(Debug, Clone)]
pub struct Expression {
    pub kind: ExpressionKind,
    pub span: Span,
}

/// Represents a pattern in a `switch` case.
#[derive(Debug, Clone)]
pub enum Pattern {
    /// A simple literal or enum value, e.g., `1`, `MyEnum.Variant`.
    Literal(Expression),
    /// A destructuring pattern, e.g., `Option.Some(value)`.
    Destructure {
        /// The full path to the variant, e.g., ("Option", "Some").
        variant_path: (String, String),
        /// The name of the variable to bind the payload to, e.g., "value".
        /// `None` if the variant has no payload or it's being ignored, e.g., `Option.None`.
        binding: Option<String>,
        span: Span,
    },
}

impl Pattern {
    /// Returns the source span of the pattern.
    pub fn span(&self) -> &Span {
        match self {
            Pattern::Literal(expr) => &expr.span,
            Pattern::Destructure { span, .. } => span,
        }
    }
}

/// An enum representing all possible kinds of expressions in the Untyped AST.
#[derive(Debug, Clone)]
pub enum ExpressionKind {
    New {
        ty: Type,
        generic_args: Vec<Type>,
        args: Vec<Arg>,
    },
    Literal(Literal),
    Identifier(String),
    BinaryOp(Box<Expression>, TokenKind, Box<Expression>),
    UnaryOp(TokenKind, Box<Expression>),
    TernaryOp(Box<Expression>, Box<Expression>, Box<Expression>),
    /// An anonymous function expression that creates a closure value.
    Closure(FunctionDecl),
    /// A function call to be evaluated at runtime.
    FunctionCall {
        name: String,
        generic_args: Vec<Type>,
        args: Vec<Expression>,
    },
    /// A function call to be evaluated at compile time.
    ConstCall {
        name: String,
        generic_args: Vec<Type>,
        args: Vec<Expression>,
    },
    InterpolatedString(Vec<InterpolationPart>),
    StructInstantiation(String, Vec<Type>, Vec<Arg>),
    TaggedUnionInstantiation {
        enum_name: String,
        variant_name: String,
        payload: Option<Box<Expression>>,
    },
    EnumVariantAccess {
        enum_name: String,
        variant_name: String,
    },
    FieldAccess(Box<Expression>, String),
    MethodCall(Box<Expression>, String, Vec<Expression>),
    Tuple(Vec<Expression>),
    TupleAccess(Box<Expression>, usize),
    AddressOf(Box<Expression>),
    Dereference(Box<Expression>),
    ArrayLiteral(Vec<Expression>),
    ArrayIndex {
        array: Box<Expression>,
        index: Box<Expression>,
    },
    ArraySlice {
        array: Box<Expression>,
        start: Option<Box<Expression>>,
        end: Option<Box<Expression>>,
    },
    DictionaryInstantiation {
        key_type: Type,
        value_type: Type,
        pairs: Vec<(Expression, Expression)>,
    },
    Switch {
        expression: Box<Expression>,
        cases: Vec<SwitchCase>,
        else_block: Option<Vec<Statement>>,
    },
    Try(Box<Expression>),
    Catch {
        expression: Box<Expression>,
        error_variable: String,
        body: Vec<Statement>,
    },
    /// An `allow` expression, used to permit potentially unsafe operations like type demotion.
    Allow(Box<Expression>),
    /// An explicit type conversion, e.g., `convert(my_int, Int32)`.
    Convert {
        expr: Box<Expression>,
        target_type: Type,
    },
    /// An explicit string parsing operation, e.g., `parse(Int64, "123")`.
    Parse {
        target_type: Type,
        expr: Box<Expression>,
    },
}

impl fmt::Display for ExpressionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExpressionKind::Literal(lit) => match lit {
                Literal::Int(i) => write!(f, "{}", i),
                Literal::Float(fl) => write!(f, "{}", fl),
                Literal::String(s) => write!(f, "\"{}\"", s),
                Literal::Char(c) => write!(f, "'{}'", c),
                Literal::Bool(b) => write!(f, "{}", b),
                Literal::Nothing => write!(f, "nothing"),
            },
            ExpressionKind::Identifier(name) => write!(f, "{}", name),
            ExpressionKind::BinaryOp(..) => write!(f, "binary expression"),
            ExpressionKind::UnaryOp(..) => write!(f, "unary expression"),
            ExpressionKind::TernaryOp(..) => write!(f, "ternary expression"),
            ExpressionKind::Closure(_) => write!(f, "closure expression"),
            ExpressionKind::FunctionCall { name, .. } => write!(f, "call to '{}'", name),
            ExpressionKind::ConstCall { name, .. } => write!(f, "const call to '{}'", name),
            ExpressionKind::StructInstantiation(name, ..) => write!(f, "instantiation of '{}'", name),
            ExpressionKind::Convert { .. } => write!(f, "convert expression"),
            ExpressionKind::Parse { .. } => write!(f, "parse expression"),
            _ => write!(f, "expression"), // Fallback for other complex kinds
        }
    }
}

/// A single target in a destructuring declaration.
/// This represents one part of the pattern, like `x: Int64`, `_`, `x: Int64 = field_x`, or `..`.
#[derive(Debug, Clone)]
pub struct DestructuringTarget {
    /// The new variable name being declared. Can be `_` to ignore the field, or `..` for a rest pattern.
    pub var_name: String,
    /// The type annotation for the new variable. This is a placeholder for `_`.
    /// This field is not used if `is_rest_pattern` is true.
    pub ty: Type,
    /// Whether the new variable is declared as mutable.
    pub is_mutable: bool,
    /// The optional source field to bind from, e.g., the `field_x` in `var: Type = field_x`.
    /// If `None`, this is a positional destructuring.
    pub source_field: Option<String>,
    /// True if this is a `..` rest pattern, meaning it discards remaining elements.
    pub is_rest_pattern: bool,
}

/// A pattern used on the left-hand side of a declaration.
#[derive(Debug, Clone)]
pub enum DeclarationPattern {
    /// A simple identifier with its type annotation, e.g., `x: Int64`.
    Identifier(String, Type),
    /// A tuple-like pattern for destructuring, e.g., `(x: Int64, mutable y: String = field_y, ..)`.
    Tuple(Vec<DestructuringTarget>),
    /// An array-like pattern for destructuring, e.g., `[x: Int64, mutable y: String, ..]`.
    Array(Vec<DestructuringTarget>),
}

/// A node in the Untyped AST representing a statement.
#[derive(Debug, Clone)]
pub struct Statement {
    pub kind: StatementKind,
    pub span: Span,
}

/// Represents the `else` or `else if` part of an `if` statement.
#[derive(Debug, Clone)]
pub enum ElseBlock {
    ElseIf(IfStatement),
    Else(Vec<Statement>),
}

/// An `if` statement in the Untyped AST.
#[derive(Debug, Clone)]
pub struct IfStatement {
    /// The expression being evaluated. For a simple `if`, this is the boolean condition.
    /// For an `if let`-style binding, this is the fallible expression.
    pub condition: Expression,
    /// If this is an `if let`-style binding, this holds the variable name and its type annotation.
    pub binding: Option<(String, Type)>,
    pub then_block: Vec<Statement>,
    pub else_block: Option<Box<ElseBlock>>,
}

/// A `while` statement in the Untyped AST.
#[derive(Debug, Clone)]
pub struct WhileStatement {
    pub condition: Expression,
    pub body: Vec<Statement>,
}

/// Represents the kind of for loop being parsed.
#[derive(Debug, Clone)]
pub enum ForLoopKind {
    /// A C-style numeric range loop, e.g., `for i = 0:10`.
    Range {
        start: Expression,
        stop: Expression,
        step: Option<Expression>,
    },
    /// An iterator-based loop, e.g., `for item in my_vector`.
    Iterable { collection: Expression },
}

/// A `for` loop in the Untyped AST. It can be a numeric range or an iterator-based loop.
#[derive(Debug, Clone)]
pub struct ForStatement {
    pub iterator_name: String,
    pub kind: ForLoopKind,
    pub body: Vec<Statement>,
}

/// A `struct` definition in the Untyped AST.
#[derive(Debug, Clone)]
pub struct StructDef {
    pub is_public: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    pub generic_params: Vec<GenericParam>,
    /// A vector of (name, type, is_public, doc_comment) tuples for each field.
    pub fields: Vec<(String, Type, bool, Option<String>)>,
    /// The path to the module file where this struct is defined.
    pub module_path: PathBuf,
}

/// An `enum` definition in the Untyped AST.
#[derive(Debug, Clone)]
pub struct EnumDef {
    pub is_public: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    /// A vector of (name, value, doc_comment) tuples for each member.
    pub members: Vec<(String, Option<i64>, Option<String>)>,
    pub module_path: PathBuf,
}

/// A `union` definition in the Untyped AST.
#[derive(Debug, Clone)]
pub struct UnionDef {
    pub is_public: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    /// A vector of (name, type, doc_comment) tuples for each field.
    pub fields: Vec<(String, Type, Option<String>)>,
    pub module_path: PathBuf,
}

/// A `tagged union` definition in the Untyped AST.
#[derive(Debug, Clone)]
pub struct TaggedUnionDef {
    pub is_public: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    pub generic_params: Vec<GenericParam>,
    pub variants: Vec<TaggedUnionVariant>,
    pub module_path: PathBuf,
}

/// An `error` definition in the Untyped AST.
#[derive(Debug, Clone)]
pub struct ErrorDef {
    pub is_public: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    pub variants: Vec<TaggedUnionVariant>,
    pub module_path: PathBuf,
}

/// A `trait` definition in the Untyped AST.
#[derive(Debug, Clone)]
pub struct TraitDef {
    pub is_public: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    pub generic_params: Vec<GenericParam>,
    pub methods: Vec<FunctionDecl>,
    pub module_path: PathBuf,
}

/// A variant within a `tagged union` or `error` definition.
#[derive(Debug, Clone)]
pub struct TaggedUnionVariant {
    pub name: String,
    pub doc_comment: Option<String>,
    /// The untyped payload, if any.
    pub payload: Option<Type>,
}

/// An `implement` block for adding methods to a struct or implementing a trait.
#[derive(Debug, Clone)]
pub struct ImplementBlock {
    pub doc_comment: Option<String>,
    pub target_type: Type,
    pub trait_type: Option<Type>,
    pub methods: Vec<FunctionDecl>,
    pub module_path: PathBuf,
}

/// A `case` within a `switch` expression. It can contain one or more patterns,
/// which can be literals or destructuring patterns.
#[derive(Debug, Clone)]
pub struct SwitchCase {
    pub patterns: Vec<Pattern>,
    pub body: Vec<Statement>,
}

/// A variable declaration in the Untyped AST. It now supports destructuring patterns.
#[derive(Debug, Clone)]
pub struct VariableDecl {
    /// The pattern for the declaration, e.g., `x: Int64` or `(x: Int64, y: String)`.
    pub pattern: DeclarationPattern,
    /// Whether this declaration is marked `const` for compile-time evaluation.
    pub is_const: bool,
    pub is_mutable: bool,
    pub initializer: Expression,
}

/// A function or method declaration in the Untyped AST.
#[derive(Debug, Clone)]
pub struct FunctionDecl {
    pub is_public: bool,
    pub is_const: bool,
    pub name: String,
    pub doc_comment: Option<String>,
    pub generic_params: Vec<GenericParam>,
    /// `true` if this is a method with a `self` parameter.
    pub has_self: bool,
    pub params: Vec<(String, Type)>,
    pub return_type: Option<Type>,
    pub body: Vec<Statement>,
    pub module_path: PathBuf,
}

/// The left-hand side of an assignment, which can be a simple expression, a tuple, or an array.
#[derive(Debug, Clone)]
pub enum AssignmentLValue {
    /// A standard l-value expression, eg., `x`, `s.field`, `arr[i]`.
    Expression(Expression),
    /// A tuple of identifiers for destructuring assignment, e.g., `(x, y)`.
    Tuple(Vec<String>),
    /// An array of identifiers for destructuring assignment, e.g., `[x, y]`.
    Array(Vec<String>),
}

/// An enum representing all possible kinds of statements in the Untyped AST.
#[derive(Debug, Clone)]
pub enum StatementKind {
    /// An `import` statement, e.g., `import my_module` or `import my_module.my_func`.
    Import {
        path: Vec<String>,
    },
    VariableDecl(VariableDecl),
    FunctionDecl(FunctionDecl),
    If(IfStatement),
    While(WhileStatement),
    For(ForStatement),
    StructDef(StructDef),
    EnumDef(EnumDef),
    UnionDef(UnionDef),
    TaggedUnionDef(TaggedUnionDef),
    ErrorDef(ErrorDef),
    TraitDef(TraitDef),
    ImplementBlock(ImplementBlock),
    Break,
    Continue,
    ExpressionStatement(Expression),
    Return(Expression),
    Assignment(AssignmentLValue, Expression),
    /// An immediate `delete` statement.
    Delete(String),
    /// A `yield` statement from a switch expression.
    Yield(Expression),
}

/// The root of the Untyped AST, representing a complete, parsed program.
pub type Program = ParsedProgram;

/// The root of the Untyped AST, representing a complete, parsed program
/// including any module-level documentation.
#[derive(Debug, Clone)]
pub struct ParsedProgram {
    pub statements: Vec<Statement>,
    pub module_doc_comment: Option<String>,
}