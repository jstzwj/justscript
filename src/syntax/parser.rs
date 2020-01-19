use std::boxed;
use std::option;

use self::super::lexer::*;
use self::super::ast::*;
use self::super::span::*;
use self::super::errors::PResult;
use self::super::diagnostic::*;
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

    pub fn parse(&mut self) -> PResult<StatementList>{
        let mut source = StatementList::new();
        loop {
            match self.parse_source_element() {
                Ok(value) => source.elements.push(value),
                Err(_) => break
            };
        }

        Ok(source)
    }

    pub fn parse_source_element(&mut self) -> PResult<StatementListItem> {
        if let Ok(statement) = self.parse_statement() {
            Ok(statement)
        } else if let Ok(function_declaration) = self.parse_function_declaration() {
            Ok(function_declaration)
        } else {
            Err(build_error(0, 0, "unknown error"))
        }
    }

    pub fn parse_statement(&mut self) -> PResult<StatementListItem> {
        if let Ok(variable_statement) = self.parse_variable_statement() {
            Ok(StatementListItem::Statement(variable_statement))
        } else if let Ok(block_statement) = self.parse_block_statement() {
            Ok(StatementListItem::Statement(block_statement))
        } else if let Ok(empty_statement) = self.parse_empty_statement() {
            Ok(StatementListItem::Statement(empty_statement))
        } else {
            Err(build_error(0, 0, "unknown error"))
        }
    }

    pub fn parse_block_statement(&mut self) -> PResult<Statement> {
        Err(build_error(0, 0, "unknown error"))
    }

    pub fn parse_variable_statement(&mut self) -> PResult<Statement> {
        Err(build_error(0, 0, "unknown error"))
    }

    pub fn parse_empty_statement(&mut self) -> PResult<Statement> {
        Err(build_error(0, 0, "unknown error"))
    }

    pub fn parse_function_declaration(&mut self) -> PResult<StatementListItem> {
        Err(build_error(0, 0, "unknown error"))
    }
}
