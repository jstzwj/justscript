//! Objects: the heap representation + property storage + hidden classes.
//!
//! The design follows V8's *hidden class* (a.k.a. *shape*) approach:
//! - a [`Shape`] describes which property names map to which slots,
//! - objects with the same property-layout sequence share the same shape,
//! - [`ObjectData`] stores the actual values in a flat slot vector, falling
//!   back to a by-name dictionary when the shape chain would grow unboundedly
//!   (the "dictionary mode").
//!
//! **Skeleton state:** shapes and slots are defined but only the dictionary
//! path is wired up; the shape-transition tree and inline caches are TODO.

pub mod property;
pub mod shape;

pub use property::{Attribute, PropertyDescriptor, PropertyKey};
pub use shape::{Shape, ShapeProperty, ShapeTransition};

use crate::value::Value;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// A shared, mutable handle to an object on the (future) GC heap.
pub type JsObject = Rc<RefCell<ObjectData>>;

/// The concrete object payload.
#[derive(Debug, Default)]
pub struct ObjectData {
    /// Hidden class / shape describing the fast-path property layout.
    pub shape: Option<Shape>,
    /// Fast-path values, indexed by the shape's slot order.
    pub slots: Vec<Value>,
    /// Slow-path / dictionary-mode properties, used when an object transitions
    /// to "dictionary mode" or stores accessors / non-standard attributes.
    pub properties: HashMap<String, PropertyDescriptor>,
    /// The internal prototype (`[[Prototype]]`), or `None` for the null proto.
    pub proto: Option<Value>,
    /// The object's class name (`[[Class]]`), e.g. `"Object"`, `"Array"`.
    pub class: &'static str,
    /// True for exotic objects such as arrays, functions, etc.
    pub is_exotic_array: bool,
    /// True for callable objects.
    pub callable: bool,
}

impl ObjectData {
    pub fn new() -> ObjectData {
        ObjectData::default()
    }

    pub fn with_proto(mut self, proto: Option<Value>) -> ObjectData {
        self.proto = proto;
        self
    }

    /// Construct a fresh `JsObject` handle.
    pub fn new_handle() -> JsObject {
        Rc::new(RefCell::new(ObjectData::new()))
    }
}

/// A user-facing object reference, with the common ergonomic constructors.
pub struct Object;

impl Object {
    pub fn new_handle() -> JsObject {
        ObjectData::new_handle()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_round_trip() {
        let o = ObjectData::new_handle();
        assert!(o.borrow().properties.is_empty());
    }
}
