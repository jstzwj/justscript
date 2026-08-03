//! Statement and declaration AST nodes.

use crate::ast::expr::{ClassDecl, Expr, FunctionDecl, ImportPhase};
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
    While {
        span: Span,
        test: Box<Expr>,
        body: Box<Stmt>,
    },
    /// A `do body while (test);` loop.
    DoWhile {
        span: Span,
        body: Box<Stmt>,
        test: Box<Expr>,
    },
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
    Labeled {
        span: Span,
        label: String,
        body: Box<Stmt>,
    },
    /// `with (obj) stmt` (sloppy mode only).
    With {
        span: Span,
        obj: Box<Expr>,
        body: Box<Stmt>,
    },
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
    Import {
        span: Span,
        spec: ImportSpec,
    },
    /// An `export` declaration (modules only).
    Export {
        span: Span,
        spec: ExportSpec,
    },
}

impl Decl {
    pub fn span(&self) -> Span {
        match self {
            Decl::Var { span, .. } | Decl::Import { span, .. } | Decl::Export { span, .. } => *span,
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
    /// `using x = expr;` — explicit resource management (disposes at scope exit).
    /// Parsed, not executed.
    Using,
    /// `await using x = expr;` — async-variant (disposes via `Symbol.asyncDispose`).
    /// Parsed, not executed.
    AwaitUsing,
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
    Bare { request: ModuleRequest },
    /// `import * as ns from "mod"`
    Namespace { ns: String, request: ModuleRequest },
    /// `import { a, b as c } from "mod"`
    Named {
        items: Vec<ImportItem>,
        request: ModuleRequest,
    },
    /// `import def from "mod"`, with an optional named or namespace import.
    Default {
        local: String,
        namespace: Option<String>,
        named: Vec<ImportItem>,
        request: ModuleRequest,
    },
}

#[derive(Clone, Debug)]
pub struct ImportItem {
    pub imported: ModuleExportName,
    pub local: String,
}

/// A normalized module request shared by static imports and indirect exports.
#[derive(Clone, Debug)]
pub struct ModuleRequest {
    pub specifier: String,
    /// Evaluation for ordinary imports/exports, defer for a deferred namespace
    /// import. Retained here so linking never has to reconstruct syntax.
    pub phase: ImportPhase,
    /// Kept in source order. Linkers must compare requests using the attribute
    /// keys and values, not source ordering.
    pub attributes: Vec<ImportAttribute>,
}

impl ModuleRequest {
    /// Whether this request targets the source phase (`import source …`).
    #[inline]
    pub fn is_source_phase(&self) -> bool {
        self.phase == ImportPhase::Source
    }

    /// Whether this request targets the defer phase (`import defer * as …`).
    #[inline]
    pub fn is_defer_phase(&self) -> bool {
        self.phase == ImportPhase::Defer
    }
}

#[derive(Clone, Debug)]
pub struct ImportAttribute {
    pub span: Span,
    pub key: String,
    pub value: String,
}

/// `ModuleExportName` deliberately retains whether the source production was
/// an IdentifierName or a StringLiteral; local named exports reject the latter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleExportName {
    Identifier(String),
    String(String),
}

impl ModuleExportName {
    pub fn value(&self) -> &str {
        match self {
            ModuleExportName::Identifier(value) | ModuleExportName::String(value) => value,
        }
    }

    pub fn is_string(&self) -> bool {
        matches!(self, ModuleExportName::String(_))
    }
}

#[derive(Clone, Debug)]
pub enum ExportSpec {
    /// `export { a, b as c }`
    Named { items: Vec<ExportItem> },
    /// `export default expr`
    Default(Expr),
    /// `export default function/class ...`
    DefaultDecl(Box<Decl>),
    /// `export * from "mod"` / `export * as name from "mod"`
    All {
        exported: Option<ModuleExportName>,
        request: ModuleRequest,
    },
    /// `export { a } from "mod"`
    ReExport {
        items: Vec<ExportItem>,
        request: ModuleRequest,
    },
    /// `export function/class/let/const ...`
    Decl(Box<Decl>),
}

#[derive(Clone, Debug)]
pub struct ExportItem {
    pub local: ModuleExportName,
    pub exported: ModuleExportName,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_request_phase_helpers_match_the_phase_field() {
        let mut request = ModuleRequest {
            specifier: "m".to_string(),
            phase: ImportPhase::Eval,
            attributes: Vec::new(),
        };
        assert!(!request.is_source_phase());
        assert!(!request.is_defer_phase());

        request.phase = ImportPhase::Source;
        assert!(request.is_source_phase());
        assert!(!request.is_defer_phase());

        request.phase = ImportPhase::Defer;
        assert!(!request.is_source_phase());
        assert!(request.is_defer_phase());
    }
}
