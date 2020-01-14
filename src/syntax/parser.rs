use std::boxed;
use std::option;

use self::super::lexer::Lexer;
use self::super::ast::*;


pub struct Parser {
    token_stream : Option<Lexer>,
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            token_stream: None
        }
    }

    pub fn parse(&mut self, code: &str) -> Source{
        self.token_stream = Some(Lexer::new(code));
        let mut source = Source::new();

        loop {
            match self.parse_source_element() {
                Some(value) => source.elements.push(value),
                None => break
            };
        }

        source
    }

    pub fn parse_source_element(&mut self) -> Option<SourceElement> {
        let ret: Option<SourceElement>;
        if let Some(statement) = self.parse_statement() {
            ret = Some(statement);
        } else if let Some(function_declaration) = self.parse_function_declaration() {
            ret = Some(function_declaration);
        } else {
            ret = None;
        }
        ret
    }

    pub fn parse_statement(&mut self) -> Option<SourceElement> {
        None
    }

    pub fn parse_function_declaration(&mut self) -> Option<SourceElement> {
        None
    }
}
