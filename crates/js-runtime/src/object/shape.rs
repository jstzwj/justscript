//! Hidden classes (shapes).

use std::sync::Arc;

/// A node in the hidden-class transition tree.
///
/// Each shape records the property added at this step plus a back-pointer to
/// its parent, so an object's full property layout is the path from the root
/// to its current shape. Two objects that gained properties in the same order
/// share a shape and thus a slot layout — enabling inline caches.
#[derive(Clone, Debug)]
pub struct Shape {
    /// The parent shape (None for the root shape).
    pub parent: Option<Arc<Shape>>,
    /// The property added by transitioning *into* this shape.
    pub property: ShapeProperty,
    /// Number of slots required (depth from the root).
    pub slot_count: u32,
}

#[derive(Clone, Debug)]
pub struct ShapeProperty {
    pub name: String,
    pub offset: u32,
    /// Attributes (writable / enumerable / configurable).
    pub attr: super::property::Attribute,
}

/// A descriptor of a shape transition: "given a parent shape and a new property
/// name, the resulting child shape".
#[derive(Clone, Debug)]
pub struct ShapeTransition {
    pub property_name: String,
    pub attr: super::property::Attribute,
}
