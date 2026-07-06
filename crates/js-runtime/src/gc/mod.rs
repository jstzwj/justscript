//! Garbage collection rooting primitives.
//!
//! **Skeleton state:** there is no real GC yet. [`Gc<T>`] is a thin wrapper
//! over `Rc<RefCell<T>>` so the rest of the runtime can be written against a
//! rooting-aware API today. When a tracing GC (or `gc`-crate integration)
//! lands, only this module changes.
//!
//! The [`Trace`] trait is defined now so that `Object`/`ObjectData` and any
//! GC-heap type can declare its child pointers up front.

pub mod root;

pub use root::{Handle, RootScope};

use std::cell::RefCell;
use std::rc::Rc;

/// A tracing hook: types that can hold GC-managed pointers describe how to
/// visit them.
///
/// Implementations call `visitor.visit(child)` for every `Gc<T>` they own.
pub trait Trace {
    fn trace(&self, visitor: &mut dyn Visitor);
}

/// A GC visitor — the tracer drives this during a collection.
pub trait Visitor {
    fn visit_value(&mut self, v: &crate::value::Value);
    fn visit_object(&mut self, o: &crate::object::JsObject);
}

/// A GC-managed pointer.
///
/// TODO(real GC): replace `Rc<RefCell<T>>` with a real GC handle. The public
/// surface (`new`, `borrow`, `clone`, pointer identity) should stay stable.
pub struct Gc<T: ?Sized>(Rc<RefCell<T>>);

impl<T> Gc<T> {
    pub fn new(value: T) -> Gc<T> {
        Gc(Rc::new(RefCell::new(value)))
    }
    pub fn borrow(&self) -> std::cell::Ref<'_, T> {
        self.0.borrow()
    }
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, T> {
        self.0.borrow_mut()
    }
    pub fn ptr_eq(a: &Gc<T>, b: &Gc<T>) -> bool {
        Rc::ptr_eq(&a.0, &b.0)
    }
}

impl<T> Clone for Gc<T> {
    fn clone(&self) -> Gc<T> {
        Gc(Rc::clone(&self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gc_basic() {
        let a = Gc::new(7i32);
        let b = a.clone();
        assert!(Gc::ptr_eq(&a, &b));
        assert_eq!(*a.borrow(), 7);
        *a.borrow_mut() = 8;
        assert_eq!(*b.borrow(), 8);
    }
}
