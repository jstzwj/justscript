//! JavaScript values.
//!
//! Today [`Value`] is a plain tagged union (`enum ValueData` inside a
//! transparent newtype). A future optimization is **NaN-boxing**: packing
//! pointers, ints and the singleton values into the 51 spare bits of a
//! quiet-NaN `f64` (à la SpiderMonkey / JSC). The public API is shaped so that
//! swap can happen behind the scenes: callers go through [`Value::data`] /
//! the constructors and never match on the raw enum.

use crate::object::{Attribute, JsObject, ObjectData, PropertyDescriptor};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// A shared ECMAScript binding cell, captured by closures and module imports.
///
/// An indirect cell is a read-only view of another binding. This models module
/// import bindings without making the exporting binding itself immutable.
#[derive(Clone)]
pub struct Cell(Rc<BindingCell>);

enum BindingCell {
    Direct(RefCell<BindingState>),
    Indirect(Cell),
}

struct BindingState {
    value: Option<Value>,
    mutable: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BindingError {
    Uninitialized,
    Immutable,
}

impl Cell {
    pub fn initialized(value: Value, mutable: bool) -> Self {
        Self(Rc::new(BindingCell::Direct(RefCell::new(BindingState {
            value: Some(value),
            mutable,
        }))))
    }

    pub fn mutable(value: Value) -> Self {
        Self::initialized(value, true)
    }

    pub fn uninitialized(mutable: bool) -> Self {
        Self(Rc::new(BindingCell::Direct(RefCell::new(BindingState {
            value: None,
            mutable,
        }))))
    }

    pub fn immutable_import(target: Cell) -> Self {
        Self(Rc::new(BindingCell::Indirect(target)))
    }

    pub fn get(&self) -> Result<Value, BindingError> {
        match self.0.as_ref() {
            BindingCell::Direct(state) => state
                .borrow()
                .value
                .clone()
                .ok_or(BindingError::Uninitialized),
            BindingCell::Indirect(target) => target.get(),
        }
    }

    /// Initialize an uninitialized direct binding, or assign an initialized
    /// mutable binding. Indirect import bindings are always immutable.
    pub fn set(&self, value: Value) -> Result<(), BindingError> {
        match self.0.as_ref() {
            BindingCell::Direct(state) => {
                let mut state = state.borrow_mut();
                if state.value.is_none() {
                    state.value = Some(value);
                    return Ok(());
                }
                if !state.mutable {
                    return Err(BindingError::Immutable);
                }
                state.value = Some(value);
                Ok(())
            }
            BindingCell::Indirect(_) => Err(BindingError::Immutable),
        }
    }

    pub fn is_initialized(&self) -> bool {
        match self.0.as_ref() {
            BindingCell::Direct(state) => state.borrow().value.is_some(),
            BindingCell::Indirect(target) => target.is_initialized(),
        }
    }

    pub fn ptr_eq(left: &Cell, right: &Cell) -> bool {
        match (left.0.as_ref(), right.0.as_ref()) {
            (BindingCell::Indirect(left), _) => Cell::ptr_eq(left, right),
            (_, BindingCell::Indirect(right)) => Cell::ptr_eq(left, right),
            _ => Rc::ptr_eq(&left.0, &right.0),
        }
    }
}

impl fmt::Debug for Cell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Ok(value) => f.debug_tuple("Cell").field(&value).finish(),
            Err(BindingError::Uninitialized) => f.write_str("Cell(<uninitialized>)"),
            Err(BindingError::Immutable) => unreachable!(),
        }
    }
}

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
    pub fn symbol(symbol: JsSymbol) -> Value {
        Value::new(ValueData::Symbol(symbol))
    }
    pub fn object(o: JsObject) -> Value {
        Value::new(ValueData::Object(o))
    }
    pub fn function(f: JsFunction) -> Value {
        Value::new(ValueData::Function(f))
    }
    pub fn generator(g: Rc<RefCell<GeneratorState>>) -> Value {
        Value::new(ValueData::Generator(g))
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
        matches!(
            self.data,
            ValueData::Object(_) | ValueData::Function(_) | ValueData::Generator(_)
        )
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
    pub fn as_function_mut(&mut self) -> Option<&mut JsFunction> {
        match &mut self.data {
            ValueData::Function(f) => Some(f),
            _ => None,
        }
    }

    pub fn is_generator(&self) -> bool {
        matches!(self.data, ValueData::Generator(_))
    }

    pub fn as_generator(&self) -> Option<&Rc<RefCell<GeneratorState>>> {
        match &self.data {
            ValueData::Generator(g) => Some(g),
            _ => None,
        }
    }

    /// If this is an object carrying a string `.name` property (as thrown Error
    /// values do), return that name. Used by the runtime conformance runner to
    /// classify `negative: { type: <Name> }` expectations.
    pub fn error_name(&self) -> Option<String> {
        let obj = match &self.data {
            ValueData::Object(o) => o,
            _ => return None,
        };
        let b = obj.borrow();
        match b.properties.get("name")? {
            crate::object::PropertyDescriptor::Data { value, .. } => match &value.data {
                ValueData::String(s) => Some(s.as_str().to_string()),
                _ => None,
            },
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
    /// A generator object: a suspended execution of a `function*` body. Holds
    /// the paused frame state; the VM checks it out on `.next()` and saves it
    /// back on `yield` / completion.
    Generator(Rc<RefCell<GeneratorState>>),
}

/// The suspended state of a generator: the pieces of a call frame that persist
/// between `.next()` calls. Lives in `js-runtime` (no `js-vm` types) so it can
/// be a `Value` variant; the VM converts to/from its `CallFrame` on resume.
#[derive(Debug)]
pub struct GeneratorState {
    pub module_index: u32,
    pub func_index: u32,
    pub pc: usize,
    pub locals: Vec<Cell>,
    pub stack: Vec<Value>,
    pub upvalues: Vec<Cell>,
    pub private_brands: HashMap<u32, u64>,
    pub this: Value,
    pub captured_this: Option<Cell>,
    pub is_async: bool,
    /// Iterator record retained while a `yield*` expression is suspended.
    pub delegate: Option<GeneratorDelegate>,
    /// Active handlers must survive suspension just like locals and the stack.
    pub try_stack: Vec<GeneratorTryState>,
    pub pending_throw: Option<Value>,
    /// `true` once the body has completed; further `.next()` returns done.
    pub done: bool,
    /// `false` until the first `.next()` (pc starts at 0).
    pub started: bool,
}

/// The completion used to resume a suspended generator.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum GeneratorResumeKind {
    Next,
    Throw,
    Return,
}

/// Persistent iterator record for the shared sync/async `yield*` state machine.
#[derive(Clone, Debug)]
pub struct GeneratorDelegate {
    pub iterator: Value,
    pub next_method: Value,
    pub async_from_sync: bool,
    pub intrinsic_next: bool,
}

/// Runtime-neutral representation of a VM exception handler.
#[derive(Clone, Debug)]
pub struct GeneratorTryState {
    pub catch_pc: Option<u16>,
    pub finally_pc: Option<u16>,
}

/// A JavaScript function value.
///
/// `id` is an opaque handle resolved by whichever backend executes the call.
/// For the interpreter it indexes into the [`js_bytecode::BytecodeModule`]
/// function table; native builtins use the `Native` variant instead.
///
/// `upvalues` are the captured local-variable cells from the function's
/// defining environment (closures), and `this_cell` optionally captures the
/// enclosing `this` for arrow functions (which have no own `this`).
#[derive(Clone, Debug)]
pub struct JsFunction {
    pub name: String,
    /// Ordinary object state carried by every callable. Keeping this handle on
    /// the function value gives functions stable object identity and lets
    /// property operations use the same descriptor machinery as objects.
    pub object: JsObject,
    /// Index of the defining bytecode module in the active runtime graph.
    pub module_index: u32,
    pub id: u32,
    pub param_count: u16,
    pub upvalues: Vec<Cell>,
    /// For arrow functions: the lexically captured `this`. `None` for ordinary
    /// functions (which get `this` from their call site).
    pub this_cell: Option<Cell>,
    /// Index into the VM's native-function table, if this is a builtin (e.g.
    /// generator `.next`). `None` for bytecode functions.
    pub native: Option<u16>,
    /// For native generator methods: the generator this `.next`/`.return`/
    /// `.throw` was extracted from. `None` for ordinary calls.
    pub bound_generator: Option<Rc<RefCell<GeneratorState>>>,
    /// Host/native functions may retain one object as internal bound state
    /// (currently Promise resolving functions).
    pub bound_object: Option<JsObject>,
    /// Bound-function internal slots.
    pub bound_this: Option<Box<Value>>,
    pub bound_args: Vec<Value>,
    /// Constructor referenced by `extends`, evaluated when the class is defined.
    pub superclass: Option<Box<Value>>,
    /// Hidden closure that evaluates this class's instance field definitions.
    pub instance_initializer: Option<Box<Value>>,
    /// Computed class element keys evaluated once per class definition. The
    /// constructor and its hidden initializers share this table.
    pub class_field_keys: Rc<RefCell<Vec<Value>>>,
    /// Runtime private environments captured by this closure, keyed by the
    /// class constructor's bytecode id in this function's module.
    pub private_brands: HashMap<u32, u64>,
    /// `true` for `function*` — calling it produces a generator object.
    pub is_generator: bool,
}

impl JsFunction {
    pub fn new(name: impl Into<String>, id: u32, param_count: u16) -> JsFunction {
        let name = name.into();
        let object = ObjectData::new_handle();
        {
            let mut data = object.borrow_mut();
            data.class = "Function";
            data.callable = true;
            data.properties.insert(
                "name".into(),
                PropertyDescriptor::Data {
                    value: Value::string(name.clone()),
                    attr: Attribute {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                },
            );
            data.properties.insert(
                "length".into(),
                PropertyDescriptor::Data {
                    value: Value::integer(i32::from(param_count)),
                    attr: Attribute {
                        writable: false,
                        enumerable: false,
                        configurable: true,
                    },
                },
            );
            data.properties.insert(
                "prototype".into(),
                PropertyDescriptor::Data {
                    value: Value::object(ObjectData::new_handle()),
                    attr: Attribute {
                        writable: true,
                        enumerable: false,
                        configurable: false,
                    },
                },
            );
        }
        JsFunction {
            name,
            object,
            module_index: 0,
            id,
            param_count,
            upvalues: Vec::new(),
            this_cell: None,
            native: None,
            bound_generator: None,
            bound_object: None,
            bound_this: None,
            bound_args: Vec::new(),
            superclass: None,
            instance_initializer: None,
            class_field_keys: Rc::new(RefCell::new(Vec::new())),
            private_brands: HashMap::new(),
            is_generator: false,
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
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct JsSymbol {
    pub id: u64,
    pub description: Option<String>,
}

impl JsSymbol {
    pub fn new(description: Option<String>) -> JsSymbol {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_ID: AtomicU64 = AtomicU64::new(4);
        JsSymbol {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            description,
        }
    }

    pub fn to_string_tag() -> JsSymbol {
        JsSymbol {
            id: 1,
            description: Some("Symbol.toStringTag".into()),
        }
    }

    pub fn iterator() -> JsSymbol {
        JsSymbol {
            id: 2,
            description: Some("Symbol.iterator".into()),
        }
    }

    pub fn async_iterator() -> JsSymbol {
        JsSymbol {
            id: 3,
            description: Some("Symbol.asyncIterator".into()),
        }
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
