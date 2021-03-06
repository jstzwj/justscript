use std::rc::Rc;
use std::rc::Weak;
use std::collections::HashMap;


#[derive(Debug, Clone)]
pub struct Value {
    header: u32,
    data: ValueData,
}

#[derive(Debug, Clone)]
pub struct ShapeProperty {
    name: String,
    offset: usize,
}

#[derive(Debug, Clone)]
pub struct Shape {
    base: Weak<Shape>,
    properties: Vec<ShapeProperty>,
}

#[derive(Debug, Clone)]
pub enum ValueData {
    Null,
    Undefined,
    Boolean(bool),
    String(String),
    Number(f64),
    Integer(i32),
    Object(Weak<Object>),
}

#[derive(Debug, Clone)]
pub struct Object {
    shape: Weak<Shape>,
    fast_properties: [Value; 8],
    properties: HashMap<String, Value>,
}