pub mod lexer;
pub mod parse;
pub mod runtime;
pub mod syntax;
pub mod interpreter;

use std::path::Path;
use crate::parse::parser::Parser;
use std::collections::HashMap;

use byteorder::{NativeEndian, WriteBytesExt};


pub struct State {
}


impl State {
    pub fn new() -> State {
        State {
        }
    }

    pub fn run(&self, script:&str) {
        let mut parser = Parser::new(script);
        match parser.parse() {
            Ok(ast) => {
                println!("{:?}", ast);
            },
            Err(_) => ()
        };
    }
}