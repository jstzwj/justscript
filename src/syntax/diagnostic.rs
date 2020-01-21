use self::super::span::{Span, BytePos};

pub enum DiagnosticMessage {
    Info(Span, String),
    Debug(Span, String),
    Error(Span, String),
    Fatal(Span, String),
}

pub fn build_info(start:usize, end:usize, msg:&str) -> DiagnosticMessage {
    DiagnosticMessage::Info(Span::make_span(start, end), String::from(msg))
}

pub fn build_debug(start:usize, end:usize, msg:&str) -> DiagnosticMessage {
    DiagnosticMessage::Debug(Span::make_span(start, end), String::from(msg))
}

pub fn build_error(start:usize, end:usize, msg:&str) -> DiagnosticMessage {
    DiagnosticMessage::Error(Span::make_span(start, end), String::from(msg))
}

pub struct Diagnostic {
    messages: Vec<DiagnosticMessage>
}

impl Diagnostic {
    pub fn new() -> Diagnostic {
        Diagnostic {
            messages: Vec::new(),
        }
    }
}