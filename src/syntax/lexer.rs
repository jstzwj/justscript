use self::super::token::*;
use self::super::span::{BytePos, CharPos, Span, SourceFile};
use self::super::span::Pos;
use crate::lexer::first_token;
use std::sync::Arc;



#[derive(Clone)]
pub struct StringReader {
    /// Initial position, read-only.
    start_pos: BytePos,
    /// The absolute offset within the source_map of the current character.
    pub pos: BytePos,
    /// Stop reading src at this index.
    end_src_index: usize,
    /// Source text to tokenize.
    src: Arc<String>,
}

impl StringReader {
    pub fn new(
        source_file: Arc<SourceFile>,
    ) -> Self {
        if source_file.src.is_none() {
            /*
            sess.span_diagnostic
                .bug(&format!("cannot lex `source_file` without source: {}", source_file.name));
            */
        }

        let src = (*source_file.src.as_ref().unwrap()).clone();

        StringReader {
            start_pos: source_file.start_pos,
            pos: source_file.start_pos,
            end_src_index: src.len(),
            src: Arc::new(src),
        }
    }

    pub fn next_token(&mut self) -> Token {
        let start_src_index = self.pos.0 as usize;
        let text: &str = &self.src[start_src_index..self.end_src_index];

        if text.is_empty() {
            return Token::new(TokenKind::EOF, Span::make_span(start_src_index, start_src_index));
        }

        let lex_token = first_token(text);
        let token = Token::new(lex_token.kind, Span::new(self.pos, self.pos + BytePos::from_usize(lex_token.len)));
        self.pos = self.pos + BytePos::from_usize(lex_token.len);
        token
    }
}