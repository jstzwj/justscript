use std::boxed;
use std::option;

use crate::parse::lexer::*;
use crate::syntax::ast::*;
use crate::syntax::span::*;
use crate::syntax::token::{TokenKind, Token};
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
        self.eat_whitespace();
        let first_token = self.token_stream.first();

        match first_token.kind {
            TokenKind::Ident => {
                let ident_name = self.token_stream.get_str(&first_token.span);
                match ident_name {
                    "var" => {
                        if let Ok(variable_statement) = self.parse_variable_statement() {
                            Ok(StatementListItem::Statement(variable_statement))
                        } else {
                            Err(())
                        }
                    },
                    "if" => {
                        if let Ok(if_statement) = self.parse_if_statement() {
                            Ok(StatementListItem::Statement(if_statement))
                        } else {
                            Err(())
                        }
                    },
                    "do" | "while" | "for" | "switch" => {
                        if let Ok(breakable_statement) = self.parse_breakable_statement() {
                            Ok(StatementListItem::Statement(breakable_statement))
                        } else {
                            Err(())
                        }
                    },
                    "continue" => {
                        Err(())
                    },
                    "return" => {
                        Err(())
                    },
                    "with" => {
                        Err(())
                    },
                    "throw" => {
                        Err(())
                    },
                    "try" => {
                        Err(())
                    },
                    _ => {
                        Err(())
                    }
                }
            },
            TokenKind::OpenBrace => {
                if let Ok(block_statement) = self.parse_block_statement() {
                    Ok(StatementListItem::Statement(block_statement))
                } else {
                    Err(())
                }
            },
            TokenKind::Semi => {
                if let Ok(empty_statement) = self.parse_empty_statement() {
                    Ok(StatementListItem::Statement(empty_statement))
                } else {
                    Err(())
                }
            },
            _ => {
                Err(())
            }
        }
    }

    pub fn parse_block_statement(&mut self) -> PResult<Statement> {
        let scope_stream = self.token_stream.clone();

        match self.token_stream.next_token().kind {
            TokenKind::OpenBrace => {
                self.eat_whitespace();
                Ok(Statement::BlockStatement)
            },
            _ => Err(())
        }
    }

    pub fn parse_variable_statement(&mut self) -> PResult<Statement> {
        let scope_stream = self.token_stream.clone();

        let mut variable_statement = VariableStatement::new();

        let var_token = self.token_stream.next_token();
        match var_token.kind {
            TokenKind::Ident => {
                if self.token_stream.get_str(&var_token.span) == "var" {
                    loop {
                        match self.parse_variable_declaration() {
                            Ok(value) => variable_statement.variable_declaration_list.push(value),
                            Err(_) => break
                        };
                    }
                    return Err(());
                } else {
                    return Err(());
                }
            },
            _ => {
                return Err(());
            }
        };
    }

    pub fn parse_if_statement(&mut self) -> PResult<Statement> {
        let semi_token = self.token_stream.first();
        match semi_token.kind {
            TokenKind::Semi => {
                self.token_stream.next_token();
                Ok(Statement::EmptyStatement)
            },
            _ => Err(())
        }
    }

    pub fn parse_breakable_statement(&mut self) -> PResult<Statement> {
        let first_token = self.token_stream.first();
        match first_token.kind {
            TokenKind::Ident => {
                match self.token_stream.get_str(&first_token.span) {
                    "for" | "while" | "do" => {
                        match self.parse_iteration_statement() {
                            Ok(breakable_statement) => Ok(Statement::BreakableStatement(breakable_statement)),
                            _ => Err(())
                        }
                    },
                    "switch" => {
                        match self.parse_switch_statement() {
                            Ok(switch_statement) => Ok(Statement::BreakableStatement(switch_statement)),
                            _ => Err(())
                        }
                    },
                    _ => Err(())
                }
            },
            _ => Err(())
        }
    }

    pub fn parse_iteration_statement(&mut self) -> PResult<BreakableStatement> {
        Err(())
    }

    pub fn parse_switch_statement(&mut self) -> PResult<BreakableStatement> {
        Err(())
    }

    pub fn parse_empty_statement(&mut self) -> PResult<Statement> {
        let semi_token = self.token_stream.first();
        match semi_token.kind {
            TokenKind::Semi => {
                self.token_stream.next_token();
                Ok(Statement::EmptyStatement)
            },
            _ => Err(())
        }
    }

    pub fn parse_variable_declaration(&mut self) -> PResult<VariableDeclaration> {
        let scope_stream = self.token_stream.clone();
        let mut variable_declaration = VariableDeclaration::new();

        match self.parse_binding_identifier() {
            Ok(value) => {
                variable_declaration.identifier = value;
            },
            Err(_) => {
                self.token_stream = scope_stream;
                return Err(());
            }
        };

        match self.token_stream.next_token().kind {
            TokenKind::Equal => (),
            _ => {
                self.token_stream = scope_stream;
                return Err(());
            }
        };

        match self.parse_assignment_expression() {
            Ok(value) => {
                variable_declaration.initializer = value;
            },
            Err(_) => {
                self.token_stream = scope_stream;
                return Err(());
            }
        };

        Ok(variable_declaration)
    }

    pub fn parse_assignment_expression(&mut self) -> PResult<AssignmentExpression> {
        Err(())
    }

    pub fn parse_left_hand_side_assignment_expression(&mut self) -> PResult<AssignmentExpression> {
        let scope_stream = self.token_stream.clone();
        let left_hand_side;
        let assignment_operator;
        let right_hand_side;

        match self.parse_left_hand_side_expression() {
            Ok(value) => {
                left_hand_side = value;
            },
            Err(_) => {
                self.token_stream = scope_stream;
                return Err(());
            }
        };

        match self.token_stream.next_token().kind {
            TokenKind::Equal => {
                assignment_operator = AssignmentOperator::Equal;
            },
            TokenKind::DivEq => {
                assignment_operator = AssignmentOperator::DivideEq;
            },
            _ => {
                self.token_stream = scope_stream;
                return Err(());
            }
        };

        match self.parse_assignment_expression() {
            Ok(value) => {
                right_hand_side = value;
            },
            Err(_) => {
                self.token_stream = scope_stream;
                return Err(());
            }
        };

        Ok(
            AssignmentExpression::LeftHandSideAssignmentExpression(
                LeftHandSideAssignmentExpression{
                    left_hand_side: Box::new(left_hand_side),
                    assignment_op: assignment_operator,
                    right_hand_side: Box::new(right_hand_side),
                }
            )
        )
    }

    pub fn parse_left_hand_side_expression(&mut self) -> PResult<LeftHandSideExpression> {
        let scope_stream = self.token_stream.clone();

        if let Ok(value) = self.parse_new_expression() {
            return Ok(LeftHandSideExpression::NewExpression(value));
        } else if let Ok(value) = self.parse_call_expression() {
            return Ok(LeftHandSideExpression::CallExpression(value));
        } else {
            self.token_stream = scope_stream;
            return Err(());
        }

    }

    pub fn parse_new_expression(&mut self) -> PResult<NewExpression> {
        Err(())
    }

    pub fn parse_call_expression(&mut self) -> PResult<CallExpression> {
        Err(())
    }

    pub fn parse_function_declaration(&mut self) -> PResult<StatementListItem> {
        Err(())
    }

    pub fn parse_binding_identifier(&mut self) -> PResult<String> {
        let token = self.token_stream.first();
        let token_str = self.token_stream.get_str(&token.span).to_string();
        if token.kind == TokenKind::Ident {
            let reserved = self.is_reserved_word();
            if (token_str == "yield") | (token_str == "await") | !reserved {
                self.token_stream.next_token();
                Ok(token_str)
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    pub fn parse_label_identifier(&mut self) -> PResult<String> {
        let token = self.token_stream.first();
        let token_str = self.token_stream.get_str(&token.span).to_string();
        if token.kind == TokenKind::Ident {
            let reserved = self.is_reserved_word();
            if (token_str == "yield") | (token_str == "await") | !reserved {
                self.token_stream.next_token();
                Ok(token_str)
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    pub fn parse_identifier_reference(&mut self) -> PResult<String> {
        let token = self.token_stream.first();
        let token_str = self.token_stream.get_str(&token.span).to_string();
        if token.kind == TokenKind::Ident {
            let reserved = self.is_reserved_word();
            if (token_str == "yield") | (token_str == "await") | !reserved {
                self.token_stream.next_token();
                Ok(token_str)
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    pub fn parse_identifier(&mut self) -> PResult<String> {
        let token = self.token_stream.first();
        let token_str = self.token_stream.get_str(&token.span).to_string();
        if token.kind == TokenKind::Ident {
            if !self.is_reserved_word() {
                self.token_stream.next_token();
                Ok(token_str)
            } else {
                Err(())
            }
        } else {
            Err(())
        }
    }

    pub fn is_ident(&mut self, token: &Token, ident_name: &str) -> bool {
        match token.kind {
            TokenKind::Ident => {
                self.token_stream.get_str(&token.span) == ident_name
            },
            _ => {
                false
            }
        }
    }

    fn is_keyword(&mut self) -> bool {
        let token = self.token_stream.first();
        match token.kind {
            TokenKind::Ident => {
                match self.token_stream.get_str(&token.span) {
                    "await" | "break" | "case" | "catch" | "class"
                    | "const" | "continue" | "debugger" | "default"
                    | "delete" | "do" | "else" | "export" | "extends"
                    | "finally" | "for" | "function" | "if" | "import"
                    | "in" | "instanceof" | "new" | "return" | "super"
                    | "switch" | "this" | "throw" | "try" | "typeof"
                    | "var" | "void" | "while" | "with" | "yield" => true,
                    _ => false
                }
            },
            _ => {
                false
            }
        }
    }

    fn is_future_reserved_word(&mut self) -> bool {
        let token = self.token_stream.first();
        match token.kind {
            TokenKind::Ident => {
                match self.token_stream.get_str(&token.span) {
                    "enum" => true,
                    _ => false
                }
            },
            _ => {
                false
            }
        }
    }

    fn is_null_literal(&mut self) -> bool {
        let token = self.token_stream.first();
        match token.kind {
            TokenKind::Ident => {
                match self.token_stream.get_str(&token.span) {
                    "null" => true,
                    _ => false
                }
            },
            _ => {
                false
            }
        }
    }

    fn is_boolean_literal(&self) -> bool {
        let token = self.token_stream.first();
        match token.kind {
            TokenKind::Ident => {
                match self.token_stream.get_str(&token.span) {
                    "true" | "false"=> true,
                    _ => false
                }
            },
            _ => {
                false
            }
        }
    }

    fn is_reserved_word(&self) -> bool {
        let token = self.token_stream.first();
        match token.kind {
            TokenKind::Ident => {
                match self.token_stream.get_str(&token.span) {
                    "await" | "break" | "case" | "catch" | "class"
                    | "const" | "continue" | "debugger" | "default"
                    | "delete" | "do" | "else" | "export" | "extends"
                    | "finally" | "for" | "function" | "if" | "import"
                    | "in" | "instanceof" | "new" | "return" | "super"
                    | "switch" | "this" | "throw" | "try" | "typeof"
                    | "var" | "void" | "while" | "with" | "yield" 
                    | "enum"
                    | "null"
                    | "true" | "false" => true,
                    _ => false
                }
            },
            _ => {
                false
            }
        }
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

    // general whitespaces
    pub fn eat_wbs(&mut self) -> bool {
        match self.token_stream.first().kind {
            TokenKind::WhiteSpace => {
                loop {
                    match self.token_stream.first().kind {
                        TokenKind::WhiteSpace | TokenKind::LineTerminator
                        | TokenKind::MultiLineComment | TokenKind::SingleLineComment=> {
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
