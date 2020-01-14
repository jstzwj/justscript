pub mod lexer;
pub mod parser;
pub mod token;
pub mod punctuator;
pub mod keyword;
pub mod ast;

pub struct BytePos(pub u32);

pub struct CharPos(pub usize);

pub fn parse(code:&str) -> ast::SourceElement {
    ast::SourceElement::FunctionDeclaration()
}