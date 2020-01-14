pub mod lexer;
pub mod parser;
pub mod token;
pub mod punctuator;
pub mod keyword;
pub mod ast;
pub mod span;

pub fn parse(code:&str) -> ast::SourceElement {
    ast::SourceElement::FunctionDeclaration()
}