//! Statement and declaration AST nodes.

use crate::ast::expr::{ClassDecl, Expr, FunctionDecl};
use crate::ast::pat::Pat;
use crate::Span;

/// A statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    /// A `{ ... }` block.
    Block { span: Span, body: Vec<Stmt> },
    /// An empty statement `;`.
    Empty(Span),
    /// A `debugger;` statement.
    Debugger(Span),
    /// An expression statement `expr;`.
    Expr { span: Span, expr: Box<Expr> },
    /// A declaration (`var`/`let`/`const`/`function`/`class`/`import`/`export`).
    Decl(Box<Decl>),
    /// `if (test) cons else alt`.
    If {
        span: Span,
        test: Box<Expr>,
        cons: Box<Stmt>,
        alt: Option<Box<Stmt>>,
    },
    /// `switch (disc) { cases }`.
    Switch {
        span: Span,
        disc: Box<Expr>,
        cases: Vec<SwitchCase>,
    },
    /// `return [expr];`.
    Return { span: Span, arg: Option<Box<Expr>> },
    /// `break [label];`.
    Break { span: Span, label: Option<String> },
    /// `continue [label];`.
    Continue { span: Span, label: Option<String> },
    /// `throw expr;`.
    Throw { span: Span, arg: Box<Expr> },
    /// `try { } catch { } finally { }`.
    Try {
        span: Span,
        block: Box<TryBlock>,
        handler: Option<Box<CatchClause>>,
        finalizer: Option<Vec<Stmt>>,
    },
    /// A `while (test) body` loop.
    While { span: Span, test: Box<Expr>, body: Box<Stmt> },
    /// A `do body while (test);` loop.
    DoWhile { span: Span, body: Box<Stmt>, test: Box<Expr> },
    /// A `for (init; test; update) body` loop.
    For {
        span: Span,
        init: Option<ForInit>,
        test: Option<Box<Expr>>,
        update: Option<Box<Expr>>,
        body: Box<Stmt>,
    },
    /// A `for (lhs of/iterable) body` loop.
    ForIn {
        span: Span,
        left: ForTarget,
        right: Box<Expr>,
        body: Box<Stmt>,
    },
    /// A `for (lhs of iterable) body` loop.
    ForOf {
        span: Span,
        left: ForTarget,
        right: Box<Expr>,
        body: Box<Stmt>,
        is_async: bool,
    },
    /// A labelled statement `label: stmt`.
    Labeled { span: Span, label: String, body: Box<Stmt> },
    /// `with (obj) stmt` (sloppy mode only).
    With { span: Span, obj: Box<Expr>, body: Box<Stmt> },
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Block { span, .. }
            | Stmt::If { span, .. }
            | Stmt::Switch { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Break { span, .. }
            | Stmt::Continue { span, .. }
            | Stmt::Throw { span, .. }
            | Stmt::Try { span, .. }
            | Stmt::While { span, .. }
            | Stmt::DoWhile { span, .. }
            | Stmt::For { span, .. }
            | Stmt::ForIn { span, .. }
            | Stmt::ForOf { span, .. }
            | Stmt::Labeled { span, .. }
            | Stmt::With { span, .. }
            | Stmt::Expr { span, .. } => *span,
            Stmt::Decl(d) => d.span(),
            Stmt::Empty(s) | Stmt::Debugger(s) => *s,
        }
    }
}

/// A declaration that may appear at statement or top level.
#[derive(Clone, Debug)]
pub enum Decl {
    Var {
        span: Span,
        kind: VarKind,
        declarations: Vec<VarDeclarator>,
    },
    Function(Box<FunctionDecl>),
    Class(Box<ClassDecl>),
    /// An `import` declaration (modules only).
    Import { span: Span, spec: ImportSpec },
    /// An `export` declaration (modules only).
    Export { span: Span, spec: ExportSpec },
}

impl Decl {
    pub fn span(&self) -> Span {
        match self {
            Decl::Var { span, .. }
            | Decl::Import { span, .. }
            | Decl::Export { span, .. } => *span,
            Decl::Function(f) => f.span,
            Decl::Class(c) => c.span,
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum VarKind {
    #[default]
    Var,
    Let,
    Const,
}

#[derive(Clone, Debug)]
pub struct VarDeclarator {
    pub span: Span,
    pub name: Pat,
    pub init: Option<Expr>,
}

#[derive(Clone, Debug)]
pub struct SwitchCase {
    pub span: Span,
    pub test: Option<Expr>, // None = `default`
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct TryBlock {
    pub span: Span,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct CatchClause {
    pub span: Span,
    pub param: Option<Pat>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub enum ForInit {
    /// `for (var x = 1; ...)`
    Var(Box<Decl>),
    /// `for (expr; ...)`
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub enum ForTarget {
    Var(Box<Decl>),
    Pat(Pat),
}

#[derive(Clone, Debug)]
pub enum ImportSpec {
    /// `import "mod"`
    Bare { source: String },
    /// `import * as ns from "mod"`
    Namespace { ns: String, source: String },
    /// `import { a, b as c } from "mod"`
    Named { items: Vec<ImportItem>, source: String },
    /// `import def from "mod"` / `import def, { a } from "mod"`
    Default {
        local: String,
        named: Vec<ImportItem>,
        source: String,
    },
}

#[derive(Clone, Debug)]
pub struct ImportItem {
    pub imported: String,
    pub local: String,
}

#[derive(Clone, Debug)]
pub enum ExportSpec {
    /// `export { a, b as c }`
    Named { items: Vec<ExportItem> },
    /// `export default expr`
    Default(Expr),
    /// `export * from "mod"`
    All { source: String },
    /// `export { a } from "mod"`
    ReExport { items: Vec<ExportItem>, source: String },
    /// `export function/class/let/const ...`
    Decl(Box<Decl>),
}

#[derive(Clone, Debug)]
pub struct ExportItem {
    pub local: String,
    pub exported: String,
}
