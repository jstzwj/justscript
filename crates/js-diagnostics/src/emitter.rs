//! Diagnostic collection and rendering.

use crate::diagnostic::{Diagnostic, DiagnosticPhase, Severity};
use js_syntax::{SourceFile, Span};

/// A sink for diagnostics.
pub trait Emitter {
    fn emit(&mut self, diag: &Diagnostic, source: Option<&SourceFile>);
}

/// Collect diagnostics into a buffer for inspection (useful in tests).
#[derive(Default)]
pub struct BufferEmitter {
    pub messages: Vec<String>,
}

impl Emitter for BufferEmitter {
    fn emit(&mut self, diag: &Diagnostic, source: Option<&SourceFile>) {
        self.messages.push(render(diag, source));
    }
}

/// Emit formatted diagnostics to stderr.
#[derive(Default)]
pub struct StderrEmitter;

impl Emitter for StderrEmitter {
    fn emit(&mut self, diag: &Diagnostic, source: Option<&SourceFile>) {
        eprintln!("{}", render(diag, source));
    }
}

/// Render a single diagnostic to a human-readable string.
pub fn render(diag: &Diagnostic, source: Option<&SourceFile>) -> String {
    let sev = match diag.severity {
        Severity::Bug => "bug",
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Note => "note",
        Severity::Help => "help",
    };
    let code = diag
        .code
        .as_deref()
        .map(|c| format!("[{}]", c))
        .unwrap_or_default();
    let phase = match diag.phase {
        DiagnosticPhase::Unspecified => "",
        DiagnosticPhase::Lex => " (lex)",
        DiagnosticPhase::Parse => " (parse)",
        DiagnosticPhase::EarlyError => " (early-error)",
        DiagnosticPhase::Compile => " (compile)",
        DiagnosticPhase::Runtime => " (runtime)",
        DiagnosticPhase::Backend => " (backend)",
        DiagnosticPhase::Internal => " (internal)",
    };
    let loc = source
        .map(|sf| {
            let (start, _) = sf.span_locs(diag.span);
            format!("{}:{}:{}: ", sf.name, start.line, start.column)
        })
        .unwrap_or_default();
    let mut out = format!("{}{}{}{}: {}", loc, sev, code, phase, diag.message);

    // Include the covered source text when we have it.
    if let Some(sf) = source {
        if !diag.span.is_dummy() && !diag.span.is_empty() {
            if let Some(snip) = diag.span.snippet(&sf.src) {
                out.push_str(&format!("\n   | `{}`", snip));
            }
        }
    }
    for note in &diag.notes {
        let note_loc = source
            .map(|sf| {
                let (start, _) = sf.span_locs(note.span);
                format!("{}:{}:{}: ", sf.name, start.line, start.column)
            })
            .unwrap_or_default();
        out.push_str(&format!("\n   = note: {}{}", note_loc, note.message));
        if let Some(sf) = source {
            if !note.span.is_dummy() && !note.span.is_empty() {
                if let Some(snip) = note.span.snippet(&sf.src) {
                    out.push_str(&format!("\n   | `{}`", snip));
                }
            }
        }
    }
    out
}

/// A convenience helper to attach to a `Span` without a message body.
pub fn span_help(span: Span) -> String {
    format!("at {:?}", span)
}
