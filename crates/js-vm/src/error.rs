//! Structured execution failures with source locations and JavaScript stacks.

use js_runtime::value::Value;
use js_syntax::{SourceFile, Span};
use std::fmt;
use std::sync::Arc;

/// One JavaScript frame captured while an exception leaves the VM.
#[derive(Clone, Debug)]
pub struct RuntimeFrame {
    pub function: String,
    pub span: Span,
    pub source: Option<Arc<SourceFile>>,
}

/// An uncaught JavaScript value. This is a language-level completion, not an
/// engine diagnostic.
#[derive(Clone, Debug)]
pub struct JsException {
    pub value: Value,
    pub source: Option<Arc<SourceFile>>,
    pub stack: Vec<RuntimeFrame>,
}

impl JsException {
    pub fn span(&self) -> Span {
        self.stack
            .first()
            .map(|frame| frame.span)
            .unwrap_or(Span::DUMMY)
    }
}

impl fmt::Display for JsException {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Uncaught {}", crate::interp::to_string(&self.value))?;
        render_stack(f, self.source.as_deref(), &self.stack)
    }
}

impl std::error::Error for JsException {}

/// A VM/backend failure, kept separate from JavaScript exceptions so callers
/// never mistake an engine limitation for user code throwing a value.
#[derive(Clone, Debug)]
pub struct EngineFault {
    pub message: String,
    pub source: Option<Arc<SourceFile>>,
    pub stack: Vec<RuntimeFrame>,
}

impl EngineFault {
    pub fn new(
        message: impl Into<String>,
        source: Option<Arc<SourceFile>>,
        stack: Vec<RuntimeFrame>,
    ) -> Self {
        Self {
            message: message.into(),
            source,
            stack,
        }
    }

    pub fn span(&self) -> Span {
        self.stack
            .first()
            .map(|frame| frame.span)
            .unwrap_or(Span::DUMMY)
    }
}

impl fmt::Display for EngineFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "engine fault: {}", self.message)?;
        render_stack(f, self.source.as_deref(), &self.stack)
    }
}

impl std::error::Error for EngineFault {}

#[derive(Clone, Debug)]
pub enum RuntimeError {
    Exception(JsException),
    Fault(EngineFault),
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::Exception(error) => error.fmt(f),
            RuntimeError::Fault(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RuntimeError {}

fn render_stack(
    f: &mut fmt::Formatter<'_>,
    source: Option<&SourceFile>,
    stack: &[RuntimeFrame],
) -> fmt::Result {
    for frame in stack {
        if let Some(source) = frame.source.as_deref().or(source) {
            let loc = source.loc(frame.span.start);
            write!(
                f,
                "\n    at {} ({}:{}:{})",
                frame.function, source.name, loc.line, loc.column
            )?;
        } else {
            write!(f, "\n    at {} ({:?})", frame.function, frame.span)?;
        }
    }
    Ok(())
}
