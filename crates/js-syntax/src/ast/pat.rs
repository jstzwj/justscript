//! Binding patterns (used in `var`/`let`/`const`, function params, catch).

use crate::Span;

/// A binding pattern: a simple identifier or a destructuring pattern.
#[derive(Clone, Debug)]
pub enum Pat {
    /// An identifier binding, e.g. `x`.
    Ident { span: Span, name: String },
    /// An array destructuring pattern, e.g. `[a, , b = 1]`.
    Array {
        span: Span,
        elements: Vec<Option<ArrayPatElement>>,
    },
    /// An object destructuring pattern, e.g. `{ a, b: c, ...rest }`.
    Object {
        span: Span,
        properties: Vec<ObjectPatProp>,
    },
    /// A rest element `...x` (valid only as the last element).
    Rest { span: Span, arg: Box<Pat> },
    /// An assignment pattern with a default, e.g. `x = 1`.
    Assignment {
        span: Span,
        left: Box<Pat>,
        right: Box<crate::ast::expr::Expr>,
    },
    /// A member assignment target inside a destructuring pattern (`[a.b] = x`,
    /// `{ a: b.c } = x`). Only valid in *assignment* destructuring, never in a
    /// binding pattern (`var`/params/catch).
    Member(Box<crate::ast::expr::MemberExpr>),
}

#[derive(Clone, Debug)]
pub enum ArrayPatElement {
    Pat(Pat),
    /// A hole (elision) in an array pattern, e.g. the middle of `[a, , b]`.
    Hole(Span),
}

#[derive(Clone, Debug)]
pub enum ObjectPatProp {
    /// Shorthand `{ a }` or keyed `{ a: b }`.
    KeyValue {
        span: Span,
        key: PropKey,
        value: Pat,
    },
    /// A rest property `{ ...rest }`.
    Rest { span: Span, arg: Box<Pat> },
}

/// A property key, used in object literals and member expressions.
#[derive(Clone, Debug)]
pub enum PropKey {
    Ident(String),
    String(String),
    Number(f64),
    /// Computed `[expr]`.
    Computed(Box<crate::ast::expr::Expr>),
    Private(String),
}

impl Pat {
    pub fn span(&self) -> Span {
        match self {
            Pat::Ident { span, .. }
            | Pat::Array { span, .. }
            | Pat::Object { span, .. }
            | Pat::Rest { span, .. }
            | Pat::Assignment { span, .. } => *span,
            Pat::Member(m) => m.span,
        }
    }
}
