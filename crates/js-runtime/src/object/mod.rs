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
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

/// A shared, mutable handle to an object on the (future) GC heap.
pub type JsObject = Rc<RefCell<ObjectData>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConstructorIdentity {
    pub module_index: u32,
    pub function_id: u32,
    pub native_id: Option<u16>,
}

/// Runtime identity of a private name. A fresh brand is allocated every time a
/// class definition is evaluated; the description only distinguishes private
/// names declared by that class.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PrivateName {
    pub brand: u64,
    pub description: String,
}

#[derive(Clone, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Clone, Debug)]
pub struct PromiseReaction {
    pub on_fulfilled: Option<Value>,
    pub on_rejected: Option<Value>,
    pub result: JsObject,
}

#[derive(Clone, Debug)]
pub struct PromiseData {
    pub state: PromiseState,
    pub reactions: Vec<PromiseReaction>,
}

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
    pub symbol_properties: HashMap<u64, PropertyDescriptor>,
    /// Private fields/methods/accessors, intentionally outside ordinary own
    /// property storage so reflection and string/symbol property operations
    /// cannot observe them.
    pub private_elements: HashMap<PrivateName, PropertyDescriptor>,
    /// Private methods/accessors created once during class evaluation and
    /// installed at that class's base/derived instance-initialization boundary.
    pub private_instance_elements: HashMap<PrivateName, PropertyDescriptor>,
    /// The internal prototype (`[[Prototype]]`), or `None` for the null proto.
    pub proto: Option<Value>,
    /// Distinguishes `Object.create(null)` from ordinary objects whose
    /// intrinsic Object prototype is still represented by VM fallback logic.
    pub explicit_null_prototype: bool,
    /// The object's class name (`[[Class]]`), e.g. `"Object"`, `"Array"`.
    pub class: &'static str,
    /// True for exotic objects such as arrays, functions, etc.
    pub is_exotic_array: bool,
    /// True for callable objects.
    pub callable: bool,
    /// Ordinary objects start extensible; namespace objects set this false.
    pub non_extensible: bool,
    /// Sorted live bindings exposed by a Module Namespace Exotic Object.
    /// Presence identifies the exotic object; namespace objects have a null
    /// prototype and are non-extensible at the VM object-operation boundary.
    pub module_namespace: Option<BTreeMap<String, crate::value::Cell>>,
    /// A deferred namespace triggers evaluation of this module index before
    /// its first observable property operation.
    pub deferred_module: Option<usize>,
    /// `Some` only for Promise instances.
    pub promise: Option<PromiseData>,
    /// Constructor and inherited constructors used by `instanceof`.
    pub constructor_chain: Vec<ConstructorIdentity>,
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

    pub fn module_namespace(exports: BTreeMap<String, crate::value::Cell>) -> JsObject {
        Self::module_namespace_with_deferred(exports, None)
    }

    pub fn module_namespace_with_deferred(
        exports: BTreeMap<String, crate::value::Cell>,
        deferred_module: Option<usize>,
    ) -> JsObject {
        let mut object = ObjectData::new();
        object.class = "Module";
        object.module_namespace = Some(exports);
        object.deferred_module = deferred_module;
        object.non_extensible = true;
        object.symbol_properties.insert(
            crate::value::JsSymbol::to_string_tag().id,
            PropertyDescriptor::Data {
                value: Value::string("Module"),
                attr: Attribute::read_only(),
            },
        );
        Rc::new(RefCell::new(object))
    }

    pub fn promise() -> JsObject {
        let mut object = ObjectData::new();
        object.class = "Promise";
        object.promise = Some(PromiseData {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        });
        Rc::new(RefCell::new(object))
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
