use self::super::token;
use self::super::{BytePos, CharPos};
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
    pub col: CharPos,
}

impl Lexer {
    pub fn new(text: &str) -> Lexer {
        Lexer {
            source: text.to_string(),
            position: BytePos(0),
            last_pos: BytePos(text.as_bytes().len() as u32),
            col: CharPos(0)
        }
    }

    fn lex(&mut self, code: &str) {}

    fn next(&mut self) -> Result<char, LexerError> {
        Ok('a')
    }

    fn peek(&mut self) -> Result<char, LexerError> {
        Ok('a')
    }
}
