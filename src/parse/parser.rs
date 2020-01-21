use std::boxed;
use std::option;

use crate::parse::lexer::*;
use crate::syntax::ast::*;
use crate::syntax::span::*;
use crate::syntax::token::TokenKind;
use crate::syntax::errors::PResult;
use crate::syntax::diagnostic::*;
use std::sync::Arc;
use crate::syntax::diagnostic::Diagnostic;


pub struct Parser {
    diagnostic_engine: Diagnostic,
    token_stream: StringReader,
}

impl Parser {
    pub fn new(code: &str) -> Parser {
        let source_file = SourceFile::new(code);
        Parser {
            diagnostic_engine: Diagnostic::new(),
            token_stream: StringReader::new(Arc::new(source_file))
        }
    }

    pub fn parse(&mut self) -> PResult<StatementList>{
        let mut source = StatementList::new();
        loop {
            match self.parse_statement_list_item() {
                Ok(value) => source.items.push(value),
                Err(_) => break
            };
        }

        Ok(source)
    }

    pub fn parse_statement_list_item(&mut self) -> PResult<StatementListItem> {
        if let Ok(statement) = self.parse_statement() {
            Ok(statement)
        } else if let Ok(function_declaration) = self.parse_function_declaration() {
            Ok(function_declaration)
        } else {
            Err(())
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
            Err(())
        }
    }

    pub fn parse_block_statement(&mut self) -> PResult<Statement> {
        Err(())
    }

    pub fn parse_variable_statement(&mut self) -> PResult<Statement> {
        let scope_stream = self.token_stream.clone();

        Err(())
    }

    pub fn parse_empty_statement(&mut self) -> PResult<Statement> {
        Err(())
    }

    pub fn parse_function_declaration(&mut self) -> PResult<StatementListItem> {
        Err(())
    }

    pub fn eat_whitespace(&mut self) -> bool {
        match self.token_stream.first().kind {
            TokenKind::WhiteSpace => {
                loop {
                    match self.token_stream.first().kind {
                        TokenKind::WhiteSpace => {
                            self.token_stream.next_token();
                        },
                        _ => {
                            break;
                        },
                    };
                };
                true
            },
            _ => false,
        }
    }
}
