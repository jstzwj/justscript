use self::super::span::Span;
pub enum DiagnosticMessage {
    Info(Span, String),
    Debug(Span, String),
    Error(Span, String),
    Fatal(Span, String),
}

pub struct Diagnostic {
    messages: Vec<DiagnosticMessage>
}