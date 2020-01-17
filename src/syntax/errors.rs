use self::super::diagnostic::DiagnosticMessage;

pub type PResult<T> = Result<T, DiagnosticMessage>;