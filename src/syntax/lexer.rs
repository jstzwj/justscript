use self::super::token::*;
use self::super::span::{BytePos, CharPos, Span, SourceFile};
use self::super::cursor::*;
use std::sync::Arc;
use std::str::Chars;
use std::error;
use std::fmt;

pub struct LexerError {
    code: i32,
    message: String,
}

impl LexerError {
    fn new(msg: &str) -> Self {
        Self {
            code: 0,
            message: msg.to_string(),
        }
    }
}

impl fmt::Display for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl fmt::Debug for LexerError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{{ file: {}, line: {} }}", file!(), line!()) // programmer-facing output
    }
}

impl error::Error for LexerError {
    fn description(&self) -> &str {
        &self.message
    }

    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        // The lower-level source of this error, if any.
        None
    }
}

/// Parses the first token from the provided input string.
pub fn first_token(input: &str) -> Token {
    debug_assert!(!input.is_empty());
    Cursor::new(input).advance_token()
}

/// Creates an iterator that produces tokens from the input string.
pub fn tokenize(mut input: &str) -> impl Iterator<Item = Token> + '_ {
    std::iter::from_fn(move || {
        if input.is_empty() {
            return None;
        }
        let token = first_token(input);
        input = &input[token.len..];
        Some(token)
    })
}

impl Cursor<'_> {
    // fn lex(&mut self, code: &str) {}
    fn eat_str(iter: &mut Chars, s: &str) -> bool {
        true
    }

    fn lex_whitespace(iter: &mut Chars) -> Option<Token> {
        let mut peekable_iter = iter.peekable();
        let c = peekable_iter.peek().unwrap();
        // WhiteSpace
        match c {
            /* <TAB> */ '\u{0009}' |
            /* <VT> */  '\u{000B}' |
            /* <FF> */  '\u{000C}' |
            /* <SP> */  '\u{0020}' |
            /* <NBSP> */'\u{00A0}' |
            /*<ZWNBSP>*/'\u{FEFF}' |
            /* <USP> */ '\u{1680}' | '\u{2000}'..='\u{200B}' | '\u{202F}' | '\u{3000}'
            => {
                iter.next();
                Some(Token::new(TokenKind::WhiteSpace, 1))
            }
            // default
            _ => {
                None
            }
        }
    }

    fn lex_lineterminator(iter: &mut Chars) -> Option<Token> {
        let mut peekable_iter = iter.peekable();
        let c = peekable_iter.peek().unwrap();
        // LineTerminator
        match c {
            /* LF */    '\u{000a}' |
            /* CR */    '\u{000d}' |
            /* LS */    '\u{2028}' |
            /* PS */    '\u{2029}'
            => {
                iter.next();
                Some(Token::new(TokenKind::LineTerminator, 1))
            },
            // default
            _ => {
                None
            }
        }
    }

    fn advance_token(&mut self) -> Token {
        let first_char = self.bump().unwrap();
        let token_kind: TokenKind = match first_char {
            _ => TokenKind::Unknown,
        };
        Token::new(token_kind, self.len_consumed())
    }
}



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
}