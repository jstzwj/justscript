//! The diagnostic data model.

use js_syntax::Span;

/// The pipeline stage that produced a diagnostic.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum DiagnosticPhase {
    /// The producer has not classified this diagnostic yet.
    #[default]
    Unspecified,
    Lex,
    Parse,
    EarlyError,
    Compile,
    Runtime,
    Backend,
    Internal,
}

/// How severe a diagnostic is.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum Severity {
    /// A bug in the engine itself.
    Bug,
    #[default]
    Error,
    Warning,
    Note,
    Help,
}

/// A secondary note attached to a primary [`Diagnostic`].
#[derive(Clone, Debug)]
pub struct Note {
    pub span: Span,
    pub message: String,
}

/// A single diagnostic message.
#[derive(Clone, Debug, Default)]
pub struct Diagnostic {
    pub severity: Severity,
    pub phase: DiagnosticPhase,
    pub span: Span,
    /// A short stable code, e.g. `"E0001"`.
    pub code: Option<String>,
    pub message: String,
    pub notes: Vec<Note>,
}

impl Diagnostic {
    pub fn new(severity: Severity, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            severity,
            phase: DiagnosticPhase::Unspecified,
            span,
            code: None,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    pub fn error(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Error, span, message)
    }

    pub fn warning(span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic::new(Severity::Warning, span, message)
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Diagnostic {
        self.code = Some(code.into());
        self
    }

    pub fn with_phase(mut self, phase: DiagnosticPhase) -> Diagnostic {
        self.phase = phase;
        self
    }

    /// Assign a phase and general code when the producing layer did not
    /// already provide more specific classification.
    pub fn classify(&mut self, phase: DiagnosticPhase, code: &'static str) {
        if self.phase == DiagnosticPhase::Unspecified {
            self.phase = phase;
        }
        if self.code.is_none() {
            self.code = Some(code.to_string());
        }
    }

    pub fn with_note(mut self, span: Span, message: impl Into<String>) -> Diagnostic {
        self.notes.push(Note {
            span,
            message: message.into(),
        });
        self
    }

    /// Whether this diagnostic counts as a hard error.
    pub fn is_error(&self) -> bool {
        matches!(self.severity, Severity::Error | Severity::Bug)
    }
}
