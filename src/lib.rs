pub mod lexer;
pub mod parse;
pub mod runtime;
pub mod syntax;
pub mod interpreter;

use std::fs;
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
    }
}
