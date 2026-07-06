//! Property descriptors and keys.

use crate::value::Value;

/// A property key: a string or a Symbol. (Numeric keys are normalized to
/// strings at insertion time.)
#[derive(Clone, Debug)]
pub enum PropertyKey {
    String(String),
    Symbol(crate::value::JsSymbol),
}

impl PropertyKey {
    pub fn from_str(s: impl Into<String>) -> PropertyKey {
        PropertyKey::String(s.into())
    }
}

/// Property attributes (`[[Writable]]`, `[[Enumerable]]`, `[[Configurable]]`).
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct Attribute {
    pub writable: bool,
    pub enumerable: bool,
    pub configurable: bool,
}

impl Attribute {
    /// Default for ordinary data properties created by assignment.
    pub fn writable() -> Attribute {
        Attribute {
            writable: true,
            enumerable: true,
            configurable: true,
        }
    }

    pub fn read_only() -> Attribute {
        Attribute {
            writable: false,
            enumerable: false,
            configurable: false,
        }
    }
}

/// A property descriptor as defined by the spec: either a data property
/// (a [`Value`]) or an accessor (get/set callables).
#[derive(Clone, Debug)]
pub enum PropertyDescriptor {
    Data { value: Value, attr: Attribute },
    Accessor {
        get: Option<Value>,
        set: Option<Value>,
        attr: Attribute,
    },
}

impl PropertyDescriptor {
    pub fn data(value: Value) -> PropertyDescriptor {
        PropertyDescriptor::Data {
            value,
            attr: Attribute::writable(),
        }
    }
}
