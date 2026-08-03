//! A *realm*: the global environment, the global object, and the root
//! prototypes that every script in that realm shares.
//!
//! Mirrors the spec's *Record* of the same name. The interpreter, JIT and AOT
//! backends all execute against a [`Realm`].

use crate::builtins;
use crate::object::JsObject;
use crate::value::Value;
use std::collections::HashMap;

pub struct Realm {
    /// The global object (`globalThis`).
    pub global_object: JsObject,
    /// Named globals cached for fast lookup during startup.
    pub globals: HashMap<String, Value>,
    /// `%ObjectPrototype%`.
    pub object_proto: Option<JsObject>,
    /// `%FunctionPrototype%`.
    pub function_proto: Option<JsObject>,
    /// `%ArrayPrototype%`.
    pub array_proto: Option<JsObject>,
    /// `%StringPrototype%`.
    pub string_proto: Option<JsObject>,
    /// `%NumberPrototype%`.
    pub number_proto: Option<JsObject>,
    /// `%BooleanPrototype%`.
    pub boolean_proto: Option<JsObject>,
    /// `%SymbolPrototype%`.
    pub symbol_proto: Option<JsObject>,
    /// `%BigIntPrototype%`.
    pub bigint_proto: Option<JsObject>,
    /// Host-observable completion signal used by Test262's async protocol.
    /// `test262_done_called` is set by `$DONE`; `test262_done_error` records the
    /// argument when `$DONE` is called with a value (an async failure). The
    /// runner must treat a non-`None` error as a test failure even if the
    /// top-level script completed normally — the throw inside a Promise
    /// reaction would otherwise be swallowed.
    pub test262_done_called: bool,
    pub test262_done_error: Option<Value>,
    /// Whether the VM-level intrinsics have been installed for this realm.
    /// `install_globals`, `globalThis`, the per-realm intrinsic prototypes and
    /// the `Array.prototype` wiring run exactly once per realm (on the first
    /// `Interpreter::new`), so a realm reused across several
    /// executes/interpreters keeps stable prototype identity and preserves user
    /// modifications to built-ins. A realm is long-lived; intrinsics must not be
    /// re-created per interpreter.
    pub intrinsics_initialized: bool,
}

impl Realm {
    /// Create a fresh realm with all built-ins installed.
    pub fn new() -> Realm {
        let mut realm = Realm {
            global_object: crate::object::ObjectData::new_handle(),
            globals: HashMap::new(),
            object_proto: None,
            function_proto: None,
            array_proto: None,
            string_proto: None,
            number_proto: None,
            boolean_proto: None,
            symbol_proto: None,
            bigint_proto: None,
            test262_done_called: false,
            test262_done_error: None,
            intrinsics_initialized: false,
        };
        builtins::install_all(&mut realm);
        realm
    }

    /// Define a global binding on the global object (and the fast cache).
    pub fn define_global(&mut self, name: impl Into<String>, value: Value) {
        self.globals.insert(name.into(), value);
    }
}

impl Default for Realm {
    fn default() -> Realm {
        Realm::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn realm_constructs() {
        let _r = Realm::new();
    }
}
