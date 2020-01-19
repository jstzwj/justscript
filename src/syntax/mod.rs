pub mod lexer;
pub mod parser;
pub mod token;
pub mod punctuator;
pub mod keyword;
pub mod ast;
pub mod span;
pub mod diagnostic;
pub mod errors;

pub fn parse(code:&str) -> ast::StatementListItem {
    ast::StatementListItem::FunctionDeclaration()
}