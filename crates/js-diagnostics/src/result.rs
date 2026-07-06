//! The `DiagResult` type — a `Result` whose error side carries diagnostics.

use crate::diagnostic::Diagnostic;

/// The outcome of a fallible compilation pass.
///
/// `Ok(value)` means the pass produced a usable result (possibly accompanied
/// by *non-fatal* diagnostics such as warnings). `Err(diagnostics)` means the
/// pass could not produce a result; the caller should report the diagnostics.
pub type DiagResult<T> = Result<T, Vec<Diagnostic>>;

/// A bag of diagnostics that may or may not contain errors.
///
/// Used by passes that want to keep going after a non-fatal error and report
/// multiple problems in one run.
#[derive(Default, Clone, Debug)]
pub struct DiagBag {
    pub diags: Vec<Diagnostic>,
}

impl DiagBag {
    pub fn new() -> DiagBag {
        DiagBag::default()
    }

    pub fn push(&mut self, diag: Diagnostic) {
        self.diags.push(diag);
    }

    pub fn extend(&mut self, other: DiagBag) {
        self.diags.extend(other.diags);
    }

    pub fn has_errors(&self) -> bool {
        self.diags.iter().any(Diagnostic::is_error)
    }

    pub fn is_empty(&self) -> bool {
        self.diags.is_empty()
    }
}

impl From<Diagnostic> for DiagBag {
    fn from(d: Diagnostic) -> DiagBag {
        DiagBag { diags: vec![d] }
    }
}

impl From<Vec<Diagnostic>> for DiagBag {
    fn from(diags: Vec<Diagnostic>) -> DiagBag {
        DiagBag { diags }
    }
}
