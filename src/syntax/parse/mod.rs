pub mod lexer;
pub mod parser;
mod token;
mod punctuator;
mod keyword;


pub struct BytePos(pub u32);

pub struct CharPos(pub usize);