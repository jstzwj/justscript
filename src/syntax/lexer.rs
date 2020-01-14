use self::super::token::*;
use self::super::span::{BytePos, CharPos};
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

pub struct Lexer {
    pub source: String,
    pub position: BytePos,
    pub last_pos: BytePos,
    pub char_position: CharPos,
}

impl Lexer {
    pub fn new(text: &str) -> Lexer {
        Lexer {
            source: text.to_string(),
            position: BytePos(0),
            last_pos: BytePos(text.as_bytes().len() as u32),
            char_position: CharPos(0)
        }
    }

    // fn lex(&mut self, code: &str) {}
    fn is_str(iter: &mut Chars, s: &str) -> bool {
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

    fn next(&mut self) -> Result<Token, LexerError> {
        let next_iter = self.source.get((self.position.0 as usize)..);
        // todo: diagnose info
        let mut iter = next_iter.unwrap().chars();

        if let Some(value) = Lexer::lex_whitespace(&mut iter) {
            Ok(value)
        } else if let Some(value) = Lexer::lex_lineterminator(&mut iter) {
            Ok(value)
        } else {
            Err(LexerError::new("Lexer error"))
        }
    }

    fn peek(&mut self) -> Result<char, LexerError> {
        Ok('a')
    }
}
