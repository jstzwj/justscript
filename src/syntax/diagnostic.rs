use self::super::span::{Span, CharPos};

pub enum DiagnosticMessage {
    Info(Span, String),
    Debug(Span, String),
    Error(Span, String),
    Fatal(Span, String),
}

pub fn build_info(start:usize, end:usize, msg:&str) -> DiagnosticMessage {
    DiagnosticMessage::Info(Span::new(CharPos(start), CharPos(end)), String::from(msg))
}

pub fn build_debug(start:usize, end:usize, msg:&str) -> DiagnosticMessage {
    DiagnosticMessage::Debug(Span::new(CharPos(start), CharPos(end)), String::from(msg))
}

pub fn build_error(start:usize, end:usize, msg:&str) -> DiagnosticMessage {
    DiagnosticMessage::Error(Span::new(CharPos(start), CharPos(end)), String::from(msg))
}

pub struct Diagnostic {
    messages: Vec<DiagnosticMessage>
}