//! The parse session: owns the source and accumulates diagnostics.

use js_syntax::SourceFile;

pub struct ParseSess {
    pub source: SourceFile,
}

impl ParseSess {
    pub fn new(source: SourceFile) -> ParseSess {
        ParseSess { source }
    }

    pub fn for_str(name: impl Into<String>, src: impl Into<std::sync::Arc<str>>) -> ParseSess {
        ParseSess::new(SourceFile::new(name, src))
    }
}
