//! The parse session: a shared, stable source identity for one parse.

use js_syntax::SourceFile;
use std::sync::Arc;

pub struct ParseSess {
    pub source: Arc<SourceFile>,
}

impl ParseSess {
    pub fn new(source: SourceFile) -> ParseSess {
        ParseSess {
            source: Arc::new(source),
        }
    }

    pub fn from_shared(source: Arc<SourceFile>) -> ParseSess {
        ParseSess { source }
    }

    pub fn for_str(name: impl Into<String>, src: impl Into<Arc<str>>) -> ParseSess {
        ParseSess::new(SourceFile::new(name, src))
    }
}
