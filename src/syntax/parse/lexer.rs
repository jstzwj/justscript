use std::fmt;

pub struct LexerError {
    code: i32,
    message: String
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
        write!(f, "{}", self.details)
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

    fn source(&self) -> Option<&(dyn Error + 'static)> {
        // The lower-level source of this error, if any.
        None
    }
}


pub struct Lexer {
    pub Vec<Token> tokens;
    pub i32 line_number;
    pub i32 column_number;
}

impl Lexer {
    fn lex(&mut self, code:&str) {

    }

    fn next(&mut self,) -> Result<char, LexerError> {

    }

    fn peek(&mut self,) -> Result<char, LexerError> {

    }
}
