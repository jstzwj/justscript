//! The JustScript abstract syntax tree.
//!
//! The AST is split across submodules by node family to keep individual files
//! scannable:
//! - [`stmt`] — statements and declarations,
//! - [`expr`] — expressions,
//! - [`lit`] — literal forms,
//! - [`pat`] — binding patterns (destructuring),
//! - [`op`] — operator enumerations shared by lexer/parser/codegen.
//!
//! Every node carries the [`Span`] it covered in the source, so diagnostics,
//! source maps and JIT/AOT debug info can all point back at exact bytes.

pub mod expr;
pub mod lit;
pub mod op;
pub mod pat;
pub mod stmt;

pub use expr::*;
pub use lit::*;
pub use op::*;
pub use pat::*;
pub use stmt::*;

use crate::Span;

/// The root of a parsed script or module.
#[derive(Clone, Debug)]
pub struct Program {
    pub span: Span,
    pub body: Vec<ProgramItem>,
    pub kind: ProgramKind,
}

impl Program {
    pub fn new(span: Span, kind: ProgramKind, body: Vec<ProgramItem>) -> Program {
        Program { span, body, kind }
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ProgramKind {
    /// A classic script (sloppy mode, top-level `var`).
    #[default]
    Script,
    /// An ES module (strict mode, `import`/`export`).
    Module,
}

/// A top-level item of a [`Program`].
#[derive(Clone, Debug)]
pub enum ProgramItem {
    Stmt(Stmt),
    Decl(Decl),
}
