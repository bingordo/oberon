//! Oberon-07 abstract syntax tree.
//!
//! Shape rationale: the tree is organised *per procedure*. A `Module` owns a
//! `Decls`; `Decls::procs` holds fully-formed `Proc` nodes, and every `Proc`
//! again owns its own `Decls` plus its body. Nested procedures therefore form
//! a tree with no side tables, and every later phase (scope/type check,
//! codegen) can be written as a fold over one `Proc` at a time.
//!
//! Syntax only: nothing here is resolved. In particular a `Selector::Args`
//! is a parenthesised group parsed after a designator which is *either* a
//! type guard `v(T0)` or the actual parameter list of a call (see
//! `Selector::Args` doc). Resolution happens in the semantic phase.

pub type Ident = String;
/// One (unqualified) or two (module-qualified) identifiers.
pub type QIdent = Vec<Ident>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(lo: usize, hi: usize) -> Span {
        Span {
            lo: lo as u32,
            hi: hi as u32,
        }
    }
    pub fn join(a: Span, b: Span) -> Span {
        Span {
            lo: a.lo,
            hi: b.hi,
        }
    }
}

/// Free function so grammar actions can stay one-liners.
pub fn sp(lo: usize, hi: usize) -> Span {
    Span::new(lo, hi)
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Module {
    pub name: Ident,
    pub imports: Vec<Import>,
    pub decls: Decls,
    pub body: Vec<Stmt>,
    pub end_name: Ident,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Import {
    pub alias: Ident,
    pub module: Ident,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Decls {
    pub consts: Vec<ConstDecl>,
    pub types: Vec<TypeDecl>,
    pub vars: Vec<VarDecl>,
    pub procs: Vec<Proc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConstDecl {
    pub name: Ident,
    pub export: bool,
    pub value: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: Ident,
    pub export: bool,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeExpr {
    /// `qualident`
    Named(QIdent, Span),
    /// `ARRAY n, m OF T`
    Array {
        dims: Vec<Expr>,
        elem: Box<TypeExpr>,
        span: Span,
    },
    /// `RECORD (Base) field: T; END`
    Record {
        base: Option<QIdent>,
        fields: Vec<FieldList>,
        span: Span,
    },
    /// `POINTER TO T`
    Pointer(Box<TypeExpr>, Span),
    /// `PROCEDURE (params): T`
    Proc(FormalParams, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldList {
    pub names: Vec<(Ident, bool)>,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VarDecl {
    pub names: Vec<(Ident, bool)>,
    pub ty: TypeExpr,
    pub span: Span,
}

/// A procedure declaration = one unit of the per-function AST.
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub name: Ident,
    pub export: bool,
    pub params: Vec<Param>,
    pub result: Option<QIdent>,
    pub decls: Decls,
    pub body: Vec<Stmt>,
    /// Oberon-07 allows exactly one `RETURN expr`, at the end of the body.
    /// The identifier repeated after `END`; must equal `name`.
    pub end_name: Ident,
    /// The span of the *name* (and its export mark), which is what a diagnostic
    /// about the declaration should point at: `span` covers the whole body.
    pub name_span: Span,
    pub span: Span,
}

/// One `FPSection` of `FormalParameters`: possibly several names sharing one
/// mode (`VAR?`) and one type.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// `(name, export_mark)`.  A mark is not allowed on a formal parameter;
    /// it is kept here so the checker can report it rather than lose it.
    pub names: Vec<(Ident, bool)>,
    pub by_ref: bool,
    pub ty: FormalType,
}

/// `{ARRAY OF} qualident`; `open` counts the leading `ARRAY OF`s.
#[derive(Debug, Clone, PartialEq)]
pub struct FormalType {
    pub open: usize,
    pub base: QIdent,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FormalParams {
    pub params: Vec<Param>,
    pub result: Option<QIdent>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtKind {
    Assign { target: Designator, value: Expr },
    /// A procedure call statement: `d` must end in `Selector::Args`.
    Call(Designator),
    If {
        arms: Vec<(Expr, Vec<Stmt>)>,
        els: Option<Vec<Stmt>>,
    },
    Case {
        subject: Expr,
        arms: Vec<CaseArm>,
    },
    While {
        arms: Vec<(Expr, Vec<Stmt>)>,
    },
    Repeat {
        body: Vec<Stmt>,
        until: Expr,
    },
    /// `EXIT` (PLAN.md W8.5): leave the innermost loop.
    Exit,
    /// `RETURN [Expr]` (PLAN.md W8.4): leave the body, with a value in a function.
    Return(Option<Expr>),
    For {
        var: Ident,
        from: Expr,
        to: Expr,
        by: Option<Expr>,
        body: Vec<Stmt>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CaseArm {
    pub labels: Option<Vec<LabelRange>>,
    pub body: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LabelRange {
    pub lo: CaseLabel,
    pub hi: Option<CaseLabel>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaseLabel {
    Int(u32),
    Char(u8),
    Str(String),
    Named(QIdent),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Designator {
    pub base: QIdent,
    pub sels: Vec<Selector>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Selector {
    Field(Ident),
    Index(Vec<Expr>),
    Deref,
    /// `( ... )` after a designator. The report makes this a *type guard*
    /// `v(T0)`, while a call's actual parameters have the same shape. Parsing
    /// both as one production keeps the grammar LR(1)-clean; `resolve.rs`
    /// rewrites it into `Guard` or `Call` (see DESIGN.md §5 D2).
    Args(Option<Vec<Expr>>),
    /// Produced by resolution only: actual parameters of a call.
    Call(Vec<Expr>),
    /// Produced by resolution only: a type guard `v(T0)`.
    Guard(QIdent),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Int(u32),
    Real(f64),
    Char(u8),
    Str(String),
    Bool(bool),
    Nil,
    Set(Vec<SetElem>),
    Designator(Designator),
    Unary(UnOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    TypeTest(Box<Expr>, QIdent),
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Expr {
        Expr { kind, span }
    }
    pub fn unary(op: UnOp, e: Expr) -> Expr {
        let span = e.span;
        Expr::new(ExprKind::Unary(op, Box::new(e)), span)
    }
    pub fn binary(op: BinOp, l: Expr, r: Expr) -> Expr {
        let span = Span::join(l.span, r.span);
        Expr::new(ExprKind::Binary(op, Box::new(l), Box::new(r)), span)
    }
}

/// Left fold used by the grammar's `{operator operand}` tiers: operators of one
/// tier associate to the left (`x-y-z` is `(x-y)-z`).
pub fn fold_ops(first: Expr, rest: Vec<(BinOp, Expr)>) -> Expr {
    rest.into_iter()
        .fold(first, |acc, (op, rhs)| Expr::binary(op, acc, rhs))
}

#[derive(Debug, Clone, PartialEq)]
pub struct SetElem {
    pub lo: Expr,
    pub hi: Option<Expr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,   // ~
    Neg,   // -
    Plus,  // +
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,   // OR
    And,  // &
    Add,  // +
    Sub,  // -
    Mul,  // *
    Div,  // /
    IDiv, // DIV
    Mod,  // MOD
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    In,
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Stmt {
        Stmt { kind, span }
    }
}
