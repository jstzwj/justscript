//! Per-realm and per-invocation execution state.

use crate::realm::Realm;
use crate::value::Value;
use std::cell::RefCell;
use std::rc::Rc;

/// Long-lived per-realm context: owns the [`Realm`] and any realm-wide caches
/// the backends share (e.g. interned strings, compiled functions).
pub struct RealmContext {
    pub realm: Rc<RefCell<Realm>>,
}

impl RealmContext {
    pub fn new(realm: Realm) -> RealmContext {
        RealmContext {
            realm: Rc::new(RefCell::new(realm)),
        }
    }

    pub fn fresh() -> RealmContext {
        RealmContext::new(Realm::new())
    }
}

impl Default for RealmContext {
    fn default() -> RealmContext {
        RealmContext::fresh()
    }
}

/// A single stack frame's execution state. The interpreter and the JIT/AOT
/// runtime both maintain a logical [`ExecutionContext`] per in-flight call so
/// that backtraces, error unwind and debugging share one representation.
#[derive(Clone, Debug, Default)]
pub struct ExecutionContext {
    /// The `this` binding for this frame.
    pub this_binding: Value,
    /// The function being executed, if any.
    pub function: Option<Value>,
    /// A user-visible function name for backtraces.
    pub function_name: String,
    /// Depth in the call stack (0 = top-level).
    pub depth: u32,
}
