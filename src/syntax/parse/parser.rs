use std::boxed;
use std::option;

use self::super::lexer::Lexer;


pub struct Parser {
    token_stream: Option<Lexer>,
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            token_stream: None
        }
    }

    pub fn parse(&mut self, code: &str) {
        self.token_stream = Some(Lexer::new(code))
    }
}
