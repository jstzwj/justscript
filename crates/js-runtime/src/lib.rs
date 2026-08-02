//! The JustScript runtime: values, objects, GC, builtins and execution realm.
//!
//! This crate owns the *runtime representation* of JavaScript values:
//! - [`value`] — the [`Value`] tagged-union (NaN-boxing is a future TODO),
//! - [`object`] — `Object`, `Shape` (hidden classes, à la V8), property storage,
//! - [`gc`] — GC rooting primitives (`Trace`, `Gc<T>`),
//! - [`builtins`] — Object/Function/Array/String/Number/Boolean/Symbol/BigInt
//!   prototypes, constructors, and a minimal `console`,
//! - [`realm`] / [`context`] — the global object and per-execution state.

pub mod builtins;
pub mod context;
pub mod gc;
pub mod object;
pub mod realm;
pub mod value;

pub use context::{ExecutionContext, RealmContext};
pub use object::{
    JsObject, Object, ObjectData, PromiseData, PromiseReaction, PromiseState, PropertyDescriptor,
    Shape,
};
pub use realm::Realm;
pub use value::{BindingError, Cell, JsBigInt, JsFunction, JsString, JsSymbol, Value, ValueData};
