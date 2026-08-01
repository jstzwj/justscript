//! Literal AST nodes.

use crate::Span;

/// A literal expression.
#[derive(Clone, Debug)]
pub enum Lit {
    Null(Span),
    Boolean(Span, bool),
    /// A numeric literal. `raw` keeps the original source text so semantic
    /// checks (e.g. legacy-octal detection in strict mode) don't lose
    /// information to the `f64` conversion.
    Number(Span, f64, String),
    /// A bigint, stored as decimal-digit text to preserve full precision.
    BigInt(Span, String),
    /// A string value plus whether its source contained a legacy octal or
    /// non-octal decimal escape, which is forbidden in strict mode.
    String(Span, String, bool),
    /// A regex literal: `pattern` + `flags`.
    Regex {
        span: Span,
        pattern: String,
        flags: String,
    },
    /// A template literal with no interpolation: `` `cooked` ``.
    TemplateString {
        span: Span,
        cooked: Option<String>,
        raw: String,
    },
}

impl Lit {
    pub fn span(&self) -> Span {
        match self {
            Lit::Null(s)
            | Lit::Boolean(s, _)
            | Lit::Number(s, _, _)
            | Lit::BigInt(s, _)
            | Lit::String(s, _, _) => *s,
            Lit::Regex { span, .. } | Lit::TemplateString { span, .. } => *span,
        }
    }
}
