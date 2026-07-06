//! Root scopes / handles.
//!
//! In a tracing GC, roots are the entry points the collector starts from.
//! These types model the rooting discipline the interpreter/codegen will
//! eventually use. For now they are thin scaffolding.

/// A scoped collection of GC roots. Drop pops all roots pushed within it.
///
/// TODO(real GC): push handles onto the thread-local root stack.
pub struct RootScope {
    _depth: usize,
}

impl RootScope {
    pub fn new() -> RootScope {
        RootScope { _depth: 0 }
    }
}

impl Default for RootScope {
    fn default() -> Self {
        RootScope::new()
    }
}

/// A rooted handle to a [`crate::value::Value`] that is guaranteed live for the
/// duration of its [`RootScope`].
#[derive(Clone, Debug)]
pub struct Handle {
    pub value: crate::value::Value,
}

impl Handle {
    pub fn new(value: crate::value::Value) -> Handle {
        Handle { value }
    }
}
