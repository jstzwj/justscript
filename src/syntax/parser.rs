use std::boxed;
use std::option;

use self::super::cursor::Cursor;
use self::super::lexer::*;
use self::super::ast::*;
use self::super::span::*;
use self::super::errors::PResult;
use std::sync::Arc;


pub struct Parser {
    token_stream: StringReader,
}

impl Parser {
    pub fn new(code: &str) -> Parser {
        let source_file = SourceFile::new(code);
        Parser {
            token_stream: StringReader::new(Arc::new(source_file))
        }
    }

    pub fn parse(&mut self) -> SourceCode{
        let mut source = SourceCode::new();
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
