//! Span-aware diagnostics for the JustScript toolchain.
//!
//! Every lexer/parser/codegen pass that can fail reports one or more
//! [`Diagnostic`]s rather than a bare `()`. They are accumulated in a
//! [`DiagCx`] and rendered with a [`Emitter`]. A [`DiagResult`] carries either
//! a successful value or the collected diagnostics (errors can be non-fatal,
//! so the parser keeps going and reports multiple at once).

pub mod diagnostic;
pub mod emitter;
pub mod result;

pub use diagnostic::{Diagnostic, DiagnosticPhase, Note, Severity};
pub use emitter::{render, BufferEmitter, Emitter, StderrEmitter};
pub use result::{DiagBag, DiagResult, DiagnosticReport};
