//! The JustScript parser: source text → [`js_syntax::ast::Program`].
//!
//! A hand-written recursive-descent parser for statements, paired with a
//! Pratt (precedence-climbing) loop for expressions. Diagnostics are reported
//! through [`js_diagnostics::DiagResult`]; the parser keeps going after a
//! non-fatal error to surface as many problems as possible in one run.
//!
//! **Skeleton state:** the public API and Pratt precedence table are in place;
//! the grammar methods are filled in incrementally.

pub mod class;
pub mod early_errors;
pub mod expr;
pub mod parser;
mod regexp;
pub mod sess;
mod static_semantics;
pub mod stmt;
pub mod token_stream;

pub use parser::{parse, parse_module, parse_script, Parser};
pub use sess::ParseSess;
pub use token_stream::ParserTokenStream;
