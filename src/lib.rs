pub mod lexer;
pub mod syntax;
pub mod vm;
pub mod tests;

use std::fs;
use std::path::Path;
use crate::syntax::parser::Parser;
use std::collections::HashMap;

use byteorder::{NativeEndian, WriteBytesExt};


pub struct State {
}


impl State {
    pub fn new() -> State {
        State {
        }
    }

    pub fn new_object(&self) -> i32 {
        0
    }

    pub fn addglobal(&self, name: &str, object: DataType)
    {

    }

    pub fn run(&self, script:&Script) {
    }
}

pub trait Data {
    fn is_undefined() -> bool;
    fn is_null() -> bool;
}


#[derive(PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SymbolType {

}

#[derive(PartialEq, PartialOrd)]
pub enum DataType {
    Undefined,
    Null,
    Boolean(bool),
    Number(f64),
    String(std::string::String),
    Object(i64),
    Symbol(SymbolType)
}

impl std::cmp::Eq for DataType {

}

impl std::cmp::Ord for DataType {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let left_type_id = match self {
            Undefined => 0,
            Null => 1,
            Boolean => 2,
            Number => 3,
            String => 4,
            Object => 5,
            Symbol => 6
        };

        let right_type_id = match other {
            Undefined => 0,
            Null => 1,
            Boolean => 2,
            Number => 3,
            String => 4,
            Object => 5,
            Symbol => 6
        };

        let mut ret = std::cmp::Ordering::Equal;

        if left_type_id != right_type_id {
            ret = left_type_id.cmp(&right_type_id);
        } else {
            match left_type_id {
                0 => ret = std::cmp::Ordering::Equal,
                1 => ret = std::cmp::Ordering::Equal,
                2 => {
                    if let DataType::Boolean(left_value) = self {
                        if let DataType::Boolean(right_value) = other {
                            ret = left_value.cmp(right_value)
                        }
                    }
                },
                3 => {
                    if let DataType::Number(left_value) = self {
                        if let DataType::Number(right_value) = other {
                            let mut left_bits = [0u8; std::mem::size_of::<f64>()];
                            let mut right_bits = [0u8; std::mem::size_of::<f64>()];
                            left_bits.as_mut().write_f64::<NativeEndian>(left_value.clone()).expect("Unable to convert from f64 to u64");
                            right_bits.as_mut().write_f64::<NativeEndian>(right_value.clone()).expect("Unable to convert from f64 to u64");
                            ret = left_bits.cmp(&right_bits)
                        }
                    }
                },
                4 => {
                    if let DataType::String(left_value) = self {
                        if let DataType::String(right_value) = other {
                            ret = left_value.cmp(right_value)
                        }
                    }
                },
                5 => {
                    if let DataType::Object(left_value) = self {
                        if let DataType::Object(right_value) = other {
                            ret = left_value.cmp(right_value)
                        }
                    }
                },
                6 => {
                    if let DataType::Symbol(left_value) = self {
                        if let DataType::Symbol(right_value) = other {
                            ret = left_value.cmp(right_value)
                        }
                    }
                },
                _ => {
                    panic!("Unknown number.");
                }
            };
        }
        ret
    }
}

impl std::hash::Hash for DataType {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            DataType::Undefined => state.write(b"DataType::Undefined"),
            DataType::Null => state.write(b"DataType::Null"),
            DataType::Boolean(value) => value.hash(state),
            DataType::Number(value) => {
                let mut bits = [0u8; std::mem::size_of::<f64>()];
                bits.as_mut().write_f64::<NativeEndian>(value.clone()).expect("Unable to convert from f64 to u64");
                bits.hash(state)
            },
            DataType::String(value) => value.hash(state),
            DataType::Object(value) => value.hash(state),
            DataType::Symbol(value) => value.hash(state),
        };
    }
}



pub struct Object {
    pub properties : HashMap<DataType, DataType>,
}

impl Object {
    pub fn new() -> Object {
        Object {
            properties: HashMap::new(),
        }
    }
}


pub struct Script {

}

impl Script {
    pub fn new() -> Script {
        Script {
        }
    }
}


pub fn compile(code:&str) -> Script{
    let mut parser = Parser::new(code);
    let element = parser.parse();
    Script::new()
}

