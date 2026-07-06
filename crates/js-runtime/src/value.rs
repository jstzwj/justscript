//! JavaScript values.
//!
//! Today [`Value`] is a plain tagged union (`enum ValueData` inside a
//! transparent newtype). A future optimization is **NaN-boxing**: packing
//! pointers, ints and the singleton values into the 51 spare bits of a
//! quiet-NaN `f64` (à la SpiderMonkey / JSC). The public API is shaped so that
//! swap can happen behind the scenes: callers go through [`Value::data`] /
//! the constructors and never match on the raw enum.

use crate::object::JsObject;

/// An opaque, cheaply-clonable JavaScript value.
///
/// `Value` is intentionally a thin wrapper: it clones in O(1) (object handles
/// are refcounted/GC-managed). Equality and type tests go through methods so
/// that the NaN-boxing rewrite doesn't break call sites.
#[derive(Clone)]
pub struct Value {
    data: ValueData,
}

impl Value {
    pub fn new(data: ValueData) -> Value {
        Value { data }
    }
    pub fn data(&self) -> &ValueData {
        &self.data
    }

    // --- constructors ----------------------------------------------------
    pub fn undefined() -> Value {
        Value::new(ValueData::Undefined)
    }
    pub fn null() -> Value {
        Value::new(ValueData::Null)
    }
    pub fn boolean(b: bool) -> Value {
        Value::new(ValueData::Boolean(b))
    }
    pub fn integer(i: i32) -> Value {
        Value::new(ValueData::Integer(i))
    }
    pub fn number(n: f64) -> Value {
        Value::new(ValueData::Number(n))
    }
    pub fn string(s: impl Into<JsString>) -> Value {
        Value::new(ValueData::String(s.into()))
    }
    pub fn object(o: JsObject) -> Value {
        Value::new(ValueData::Object(o))
    }
    pub fn function(f: JsFunction) -> Value {
        Value::new(ValueData::Function(f))
    }

    // --- type tests ------------------------------------------------------
    pub fn is_undefined(&self) -> bool {
        matches!(self.data, ValueData::Undefined)
    }
    pub fn is_null(&self) -> bool {
        matches!(self.data, ValueData::Null)
    }
    pub fn is_nullish(&self) -> bool {
        self.is_null() || self.is_undefined()
    }
    pub fn is_object(&self) -> bool {
        matches!(self.data, ValueData::Object(_))
    }
    pub fn is_function(&self) -> bool {
        matches!(self.data, ValueData::Function(_))
    }
    pub fn as_function(&self) -> Option<&JsFunction> {
        match &self.data {
            ValueData::Function(f) => Some(f),
            _ => None,
        }
    }
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.data, f)
    }
}

impl Default for Value {
    fn default() -> Value {
        Value::undefined()
    }
}

/// The concrete payload of a [`Value`].
///
/// TODO(future): replace `Value { data: ValueData }` with a NaN-boxed `u64`.
#[derive(Clone, Debug)]
pub enum ValueData {
    Undefined,
    Null,
    Boolean(bool),
    /// A small integer. Kept separate from `Number` so the interpreter can
    /// skip floating-point math for the common case.
    Integer(i32),
    Number(f64),
    String(JsString),
    Symbol(JsSymbol),
    BigInt(JsBigInt),
    Object(JsObject),
    /// A callable function. For bytecode functions, [`JsFunction::id`] is the
    /// index used by the VM's function table (0 = top-level `<main>`).
    Function(JsFunction),
}

/// A JavaScript function value.
///
/// `id` is an opaque handle resolved by whichever backend executes the call.
/// For the interpreter it indexes into the [`js_bytecode::BytecodeModule`]
/// function table; native builtins use the `Native` variant instead.
#[derive(Clone, Debug)]
pub struct JsFunction {
    pub name: String,
    pub id: u32,
    pub param_count: u16,
}

impl JsFunction {
    pub fn new(name: impl Into<String>, id: u32, param_count: u16) -> JsFunction {
        JsFunction {
            name: name.into(),
            id,
            param_count,
        }
    }
}

// --- string / symbol / bigint --------------------------------------------

/// A JavaScript string is a sequence of UTF-16 code units. We use Rust `String`
/// (UTF-8) for storage in the skeleton; lone surrogates are a known limitation.
#[derive(Clone, Debug, Default)]
pub struct JsString(pub String);

impl JsString {
    pub fn new(s: impl Into<String>) -> JsString {
        JsString(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for JsString {
    fn from(s: String) -> JsString {
        JsString(s)
    }
}

impl From<&str> for JsString {
    fn from(s: &str) -> JsString {
        JsString(s.to_string())
    }
}

impl std::ops::Deref for JsString {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl PartialEq for JsString {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for JsString {}

/// A unique Symbol value. Identity is by address (the `Arc` pointer), matching
/// the spec's "distinct symbol" semantics.
#[derive(Clone, Debug)]
pub struct JsSymbol {
    pub description: Option<String>,
}

impl JsSymbol {
    pub fn new(description: Option<String>) -> JsSymbol {
        JsSymbol { description }
    }
}

/// An arbitrary-precision integer.
#[derive(Clone, Debug, Default)]
pub struct JsBigInt(pub String);

impl JsBigInt {
    pub fn from_i64(n: i64) -> JsBigInt {
        JsBigInt(n.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructors_and_tests() {
        assert!(Value::undefined().is_undefined());
        assert!(Value::null().is_null());
        assert!(!Value::integer(5).is_object());
        // String value round-trips through the constructor.
        match Value::string("hi").data() {
            ValueData::String(s) => assert_eq!(s.as_str(), "hi"),
            other => panic!("expected string, got {other:?}"),
        }
    }
}
