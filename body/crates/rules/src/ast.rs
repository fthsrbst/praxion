//! The rule-list syntax tree.
//!
//! A ruleset is a list of [`Skill`]s. A skill has typed parameters, an optional
//! documentation string and an ordered list of [`Rule`]s (`guard -> action
//! [until guard] [max_ticks n]`). Rules are ordered highest priority first.
//!
//! Every node carries a [`Span`]. Spans are deliberately ignored by
//! `PartialEq` so a pretty-printed and re-parsed tree compares equal to the
//! original; the round-trip test in `print.rs` relies on that.

use std::fmt;

/// A one-based position in a source file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Pos {
    pub line: u32,
    pub col: u32,
}

impl Pos {
    pub const fn new(line: u32, col: u32) -> Self {
        Self { line, col }
    }
}

impl fmt::Display for Pos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// A half-open source range, from the first character of a token to the last
/// character after it.
#[derive(Clone, Copy, Debug, Hash)]
pub struct Span {
    pub start: Pos,
    pub end: Pos,
}

impl Span {
    pub const fn new(start: Pos, end: Pos) -> Self {
        Self { start, end }
    }

    pub const fn empty(at: Pos) -> Self {
        Self { start: at, end: at }
    }
}

/// Spans do not take part in AST equality: `parse(print(ast)) == ast` compares
/// structure, not source positions.
impl PartialEq for Span {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for Span {}

/// The type of a skill parameter or of a fact's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueType {
    Int,
    Item,
    Block,
    EntityKind,
    Bool,
}

impl ValueType {
    /// Parses the five type keywords of the DSL.
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "int" => Some(Self::Int),
            "item" => Some(Self::Item),
            "block" => Some(Self::Block),
            "entity_kind" => Some(Self::EntityKind),
            "bool" => Some(Self::Bool),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Int => "int",
            Self::Item => "item",
            Self::Block => "block",
            Self::EntityKind => "entity_kind",
            Self::Bool => "bool",
        }
    }
}

/// The type a primitive argument slot expects.
///
/// `BlockOrEntity` exists for `move_to`, whose single target may name either a
/// block or an entity kind. The callee decides which one at run time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ArgType {
    Value(ValueType),
    BlockOrEntity,
    Fact,
}

impl ArgType {
    pub const fn value(self) -> Option<ValueType> {
        match self {
            Self::Value(t) => Some(t),
            Self::BlockOrEntity | Self::Fact => None,
        }
    }
}

impl fmt::Display for ArgType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(t) => f.write_str(t.name()),
            Self::BlockOrEntity => f.write_str("block or entity_kind"),
            Self::Fact => f.write_str("fact"),
        }
    }
}

/// One concrete argument at run time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgValue {
    Int(i64),
    Item(String),
    Block(String),
    EntityKind(String),
    Bool(bool),
    /// A block-or-entity identifier whose kind only the primitive decides.
    Name(String),
    /// A fact call, used by the `observe` primitive.
    Fact(FactCall),
}

impl ArgValue {
    /// The identifier of a name-carrying value, if it has one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Item(n) | Self::Block(n) | Self::EntityKind(n) | Self::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            _ => None,
        }
    }
}

/// A concrete fact call at run time: its name and resolved arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactCall {
    pub name: String,
    pub args: Vec<ArgValue>,
}

impl fmt::Display for FactCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}(", self.name)?;
        for (i, arg) in self.args.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{arg}")?;
        }
        f.write_str(")")
    }
}

impl fmt::Display for ArgValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Int(n) => write!(f, "{n}"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Item(n) | Self::Block(n) | Self::EntityKind(n) | Self::Name(n) => f.write_str(n),
            Self::Fact(call) => write!(f, "{call}"),
        }
    }
}

/// A declared skill parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: ValueType,
    pub default: Option<ArgValue>,
    pub span: Span,
}

/// One skill of a ruleset.
#[derive(Clone, Debug)]
pub struct Skill {
    pub name: String,
    pub params: Vec<Param>,
    pub doc: Option<String>,
    pub rules: Vec<Rule>,
    pub span: Span,
    /// Set by [`crate::load_files`] so load errors can name their file.
    pub file: Option<String>,
}

/// Equality is structural; the source position and file are not part of it, so
/// `parse(print(ast)) == ast` also holds for skills loaded from several files.
impl PartialEq for Skill {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.params == other.params
            && self.doc == other.doc
            && self.rules == other.rules
    }
}

impl Skill {
    pub fn param_index(&self, name: &str) -> Option<usize> {
        self.params.iter().position(|p| p.name == name)
    }

    /// Number of parameters without a default.
    pub fn required_params(&self) -> usize {
        self.params.iter().take_while(|p| p.default.is_none()).count()
    }
}

/// One `guard -> action` rule.
#[derive(Clone, Debug, PartialEq)]
pub struct Rule {
    pub guard: Expr,
    pub action: Action,
    pub until: Option<Expr>,
    pub max_ticks: Option<u32>,
    pub span: Span,
}

/// A guard expression.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Bool(bool),
    Int(i64),
    /// `$name`: a skill parameter.
    Param(String),
    /// A bare identifier: an item, block or entity kind, decided by the
    /// expected argument type.
    Name(String),
    Call {
        name: String,
        args: Vec<Expr>,
    },
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    Cmp {
        op: CmpOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CmpOp {
    Lt,
    Le,
    Eq,
    Ne,
    Ge,
    Gt,
}

impl CmpOp {
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Ge => ">=",
            Self::Gt => ">",
        }
    }
}

/// What a rule does when its guard is true.
#[derive(Clone, Debug, PartialEq)]
pub struct Action {
    pub kind: ActionKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ActionKind {
    Done,
    Fail(String),
    Primitive { name: String, args: Vec<Expr> },
    Do { skill: String, args: Vec<Expr> },
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }
}

impl Action {
    pub fn new(kind: ActionKind, span: Span) -> Self {
        Self { kind, span }
    }
}
