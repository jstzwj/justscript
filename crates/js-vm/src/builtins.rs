//! Built-in instance methods (Array, String) implemented as native functions.
//!
//! Each method has a stable id; [`builtin_method_id`] resolves `(receiver, name)`
//! to an id so `GetProp` can return a bound native. The [`Builtin`] dispatcher
//! implements [`NativeFn`] and matches on the id. Callback-taking methods
//! (`map`/`filter`/`forEach`) are deferred — they need native→JS sub-dispatch.

use crate::interp::{
    array_get, array_len, eq_strict, make_array, to_string, BytecodeGraph, InterpError,
    Interpreter, NativeFn, NativeResult,
};
use js_runtime::object::{
    Attribute, JsObject, PromiseData, PromiseReaction, PromiseState, PropertyDescriptor,
};
use js_runtime::value::{JsFunction, Value, ValueData};

/// Builtin method ids (must follow the generator ids 0..=2 in `default_natives`).
pub mod id {
    pub const ARR_PUSH: u16 = 3;
    pub const ARR_POP: u16 = 4;
    pub const ARR_SHIFT: u16 = 5;
    pub const ARR_JOIN: u16 = 6;
    pub const ARR_SLICE: u16 = 7;
    pub const ARR_CONCAT: u16 = 8;
    pub const ARR_INDEX_OF: u16 = 9;
    pub const ARR_INCLUDES: u16 = 10;
    pub const ARR_REVERSE: u16 = 11;
    pub const STR_CHAR_AT: u16 = 12;
    pub const STR_CHAR_CODE_AT: u16 = 13;
    pub const STR_SLICE: u16 = 14;
    pub const STR_SUBSTRING: u16 = 15;
    pub const STR_INDEX_OF: u16 = 16;
    pub const STR_INCLUDES: u16 = 17;
    pub const STR_UPPER: u16 = 18;
    pub const STR_LOWER: u16 = 19;
    pub const STR_TRIM: u16 = 20;
    pub const STR_REPEAT: u16 = 21;
    pub const STR_SPLIT: u16 = 22;
    pub const STR_CONCAT: u16 = 23;
    pub const ARR_MAP: u16 = 24;
    pub const ARR_FILTER: u16 = 25;
    pub const ARR_FOR_EACH: u16 = 26;
    pub const ARR_REDUCE: u16 = 27;
    pub const ARR_FIND: u16 = 28;
    pub const ARR_SOME: u16 = 29;
    pub const ARR_EVERY: u16 = 30;
    // Globals: console, Math, Object, JSON, coercion, etc.
    pub const CONSOLE_LOG: u16 = 31;
    pub const CONSOLE_ERROR: u16 = 32;
    pub const CONSOLE_WARN: u16 = 33;
    pub const MATH_MAX: u16 = 34;
    pub const MATH_MIN: u16 = 35;
    pub const MATH_ABS: u16 = 36;
    pub const MATH_FLOOR: u16 = 37;
    pub const MATH_CEIL: u16 = 38;
    pub const MATH_ROUND: u16 = 39;
    pub const MATH_SQRT: u16 = 40;
    pub const MATH_POW: u16 = 41;
    pub const MATH_SIGN: u16 = 42;
    pub const OBJECT_KEYS: u16 = 43;
    pub const OBJECT_VALUES: u16 = 44;
    pub const OBJECT_ENTRIES: u16 = 45;
    pub const OBJECT_ASSIGN: u16 = 46;
    pub const JSON_STRINGIFY: u16 = 47;
    pub const PARSE_INT: u16 = 48;
    pub const PARSE_FLOAT: u16 = 49;
    pub const IS_NAN: u16 = 50;
    pub const IS_FINITE: u16 = 51;
    pub const NUMBER_FN: u16 = 52;
    pub const STRING_FN: u16 = 53;
    pub const BOOLEAN_FN: u16 = 54;
    pub const ARRAY_IS_ARRAY: u16 = 55;
    pub const ARR_SORT: u16 = 56;
    pub const ARR_FLAT: u16 = 57;
    pub const ARR_FILL: u16 = 58;
    pub const ARR_AT: u16 = 59;
    pub const STR_PAD_START: u16 = 60;
    pub const STR_PAD_END: u16 = 61;
    pub const STR_STARTS_WITH: u16 = 62;
    pub const STR_ENDS_WITH: u16 = 63;
    pub const STR_REPLACE: u16 = 64;
    pub const STR_AT: u16 = 65;
    pub const ERROR_CTOR: u16 = 66;
    pub const TYPE_ERROR_CTOR: u16 = 67;
    pub const RANGE_ERROR_CTOR: u16 = 68;
    pub const SYNTAX_ERROR_CTOR: u16 = 69;
    pub const REF_ERROR_CTOR: u16 = 70;
    pub const JSON_PARSE: u16 = 71;
    pub const NUM_TO_FIXED: u16 = 72;
    pub const NUM_TO_STRING: u16 = 73;
    pub const NUM_IS_INTEGER: u16 = 74;
    pub const NUM_IS_FINITE: u16 = 75;
    pub const NUM_IS_NAN: u16 = 76;
    pub const NUM_PARSE_INT: u16 = 77;
    pub const NUM_PARSE_FLOAT: u16 = 78;
    pub const STR_TRIM_START: u16 = 79;
    pub const STR_TRIM_END: u16 = 80;
    pub const STR_FROM_CHAR_CODE: u16 = 81;
    pub const STR_CODE_POINT_AT: u16 = 82;
    pub const REGEX_TEST: u16 = 83;
    pub const REGEX_EXEC: u16 = 84;
    pub const STR_MATCH: u16 = 85;
    pub const STR_SEARCH: u16 = 86;
    // test262 harness (assert.*, Test262Error, $DONE). Installed on demand via
    // `install_test262_harness`, not present in a plain realm.
    pub const TEST262_ERROR_CTOR: u16 = 87;
    pub const ASSERT_SAME_VALUE: u16 = 88;
    pub const ASSERT_NOT_SAME_VALUE: u16 = 89;
    pub const ASSERT_THROWS: u16 = 90;
    pub const DONE: u16 = 91;
    pub const PROMISE_CTOR: u16 = 92;
    pub const PROMISE_RESOLVE: u16 = 93;
    pub const PROMISE_REJECT: u16 = 94;
    pub const PROMISE_THEN: u16 = 95;
    pub const PROMISE_CATCH: u16 = 96;
    pub const PROMISE_RESOLVING_FULFILL: u16 = 97;
    pub const PROMISE_RESOLVING_REJECT: u16 = 98;
    pub const ARRAY_CTOR: u16 = 99;
    pub const MAP_CTOR: u16 = 100;
    pub const SET_CTOR: u16 = 101;
    pub const WRAPPER_VALUE_OF: u16 = 102;
    pub const SYMBOL_FN: u16 = 103;
    pub const SYMBOL_TO_STRING: u16 = 104;
    pub const FUNCTION_CALL: u16 = 105;
    pub const OBJECT_HAS_OWN: u16 = 106;
    pub const OBJECT_PROP_ENUM: u16 = 107;
    pub const OBJECT_GET_PROTO: u16 = 108;
    pub const OBJECT_SET_PROTO: u16 = 109;
    pub const OBJECT_IS_EXTENSIBLE: u16 = 110;
    pub const OBJECT_PREVENT_EXTENSIONS: u16 = 111;
    pub const OBJECT_GET_OWN_DESC: u16 = 112;
    pub const OBJECT_DEFINE_PROP: u16 = 113;
    pub const OBJECT_GET_OWN_NAMES: u16 = 114;
    pub const OBJECT_GET_OWN_SYMBOLS: u16 = 115;
    pub const OBJECT_FREEZE: u16 = 116;
    pub const OBJECT_IS_FROZEN: u16 = 117;
    pub const REFLECT_DEFINE_PROP: u16 = 118;
    pub const REFLECT_DELETE_PROP: u16 = 119;
    pub const REFLECT_HAS: u16 = 120;
    pub const REFLECT_PREVENT_EXTENSIONS: u16 = 121;
    pub const REFLECT_SET: u16 = 122;
    pub const REFLECT_OWN_KEYS: u16 = 123;
    pub const REFLECT_GET: u16 = 124;
    pub const ASSERT: u16 = 125;
    pub const FUNCTION_BIND: u16 = 126;
    pub const EVAL: u16 = 127;
    pub const OBJECT_CREATE: u16 = 128;
    /// One-past-the-last id (for registering the dispatch table).
    pub const COUNT: u16 = 129;
}

/// Resolve a static method on a global constructor function (Number/String).
pub fn native_static_id(obj: &Value, name: &str) -> Option<u16> {
    use id::*;
    let f = obj.as_function()?;
    match f.native? {
        NUMBER_FN => Some(match name {
            "isInteger" => NUM_IS_INTEGER,
            "isFinite" => NUM_IS_FINITE,
            "isNaN" => NUM_IS_NAN,
            "parseInt" => NUM_PARSE_INT,
            "parseFloat" => NUM_PARSE_FLOAT,
            _ => return None,
        }),
        STRING_FN => Some(match name {
            "fromCharCode" => STR_FROM_CHAR_CODE,
            _ => return None,
        }),
        PROMISE_CTOR => Some(match name {
            "resolve" => PROMISE_RESOLVE,
            "reject" => PROMISE_REJECT,
            _ => return None,
        }),
        ARRAY_CTOR => Some(match name {
            "isArray" => ARRAY_IS_ARRAY,
            _ => return None,
        }),
        _ => None,
    }
}

pub fn native_static_value(obj: &Value, name: &str) -> Option<Value> {
    let function = obj.as_function()?;
    if function.native == Some(id::PROMISE_CTOR) && name == "prototype" {
        return Some(promise_prototype());
    }
    if function.native == Some(id::SYMBOL_FN) {
        return match name {
            "toStringTag" => Some(Value::symbol(js_runtime::value::JsSymbol::to_string_tag())),
            "iterator" => Some(Value::symbol(js_runtime::value::JsSymbol::iterator())),
            "asyncIterator" => Some(Value::symbol(js_runtime::value::JsSymbol::async_iterator())),
            _ => None,
        };
    }
    None
}

/// Resolve a method name on a receiver to a builtin id, if any.
pub fn builtin_method_id(this: &Value, name: &str) -> Option<u16> {
    use id::*;
    match this.data() {
        ValueData::Function(_) => Some(match name {
            "call" => FUNCTION_CALL,
            "bind" => FUNCTION_BIND,
            _ => return None,
        }),
        ValueData::Symbol(_) => Some(match name {
            "toString" => SYMBOL_TO_STRING,
            _ => return None,
        }),
        ValueData::Object(o) if o.borrow().is_exotic_array => Some(match name {
            "push" => ARR_PUSH,
            "pop" => ARR_POP,
            "shift" => ARR_SHIFT,
            "join" => ARR_JOIN,
            "slice" => ARR_SLICE,
            "concat" => ARR_CONCAT,
            "indexOf" => ARR_INDEX_OF,
            "includes" => ARR_INCLUDES,
            "reverse" => ARR_REVERSE,
            "map" => ARR_MAP,
            "filter" => ARR_FILTER,
            "forEach" => ARR_FOR_EACH,
            "reduce" => ARR_REDUCE,
            "find" => ARR_FIND,
            "some" => ARR_SOME,
            "every" => ARR_EVERY,
            "sort" => ARR_SORT,
            "flat" => ARR_FLAT,
            "fill" => ARR_FILL,
            "at" => ARR_AT,
            _ => return None,
        }),
        ValueData::Object(o) if o.borrow().class == "RegExp" => Some(match name {
            "test" => REGEX_TEST,
            "exec" => REGEX_EXEC,
            _ => return None,
        }),
        ValueData::Object(o) if o.borrow().class == "Promise" => Some(match name {
            "then" => PROMISE_THEN,
            "catch" => PROMISE_CATCH,
            _ => return None,
        }),
        ValueData::Object(o) if matches!(o.borrow().class, "Number" | "String" | "Boolean") => {
            Some(match name {
                "valueOf" => WRAPPER_VALUE_OF,
                _ => return None,
            })
        }
        ValueData::String(_) => Some(match name {
            "toString" => SYMBOL_TO_STRING,
            "charAt" => STR_CHAR_AT,
            "charCodeAt" => STR_CHAR_CODE_AT,
            "codePointAt" => STR_CODE_POINT_AT,
            "slice" => STR_SLICE,
            "substring" => STR_SUBSTRING,
            "indexOf" => STR_INDEX_OF,
            "includes" => STR_INCLUDES,
            "toUpperCase" => STR_UPPER,
            "toLowerCase" => STR_LOWER,
            "trim" => STR_TRIM,
            "trimStart" => STR_TRIM_START,
            "trimEnd" => STR_TRIM_END,
            "repeat" => STR_REPEAT,
            "split" => STR_SPLIT,
            "concat" => STR_CONCAT,
            "padStart" => STR_PAD_START,
            "padEnd" => STR_PAD_END,
            "startsWith" => STR_STARTS_WITH,
            "endsWith" => STR_ENDS_WITH,
            "replace" => STR_REPLACE,
            "match" => STR_MATCH,
            "search" => STR_SEARCH,
            "at" => STR_AT,
            _ => return None,
        }),
        ValueData::Integer(_) | ValueData::Number(_) => Some(match name {
            "toFixed" => NUM_TO_FIXED,
            "toString" => NUM_TO_STRING,
            _ => return None,
        }),
        _ => None,
    }
}

/// Build the builtin dispatch table (one `Builtin` per id).
pub fn all_builtins() -> Vec<Box<dyn NativeFn>> {
    use id::*;
    let mut v: Vec<Box<dyn NativeFn>> = Vec::new();
    for bid in ARR_PUSH..COUNT {
        v.push(Box::new(Builtin { id: bid }));
    }
    v
}

struct Builtin {
    id: u16,
}

impl NativeFn for Builtin {
    fn call(
        &self,
        interp: &mut Interpreter,
        module: &BytecodeGraph<'_>,
        this: Value,
        _f: &JsFunction,
        args: Vec<Value>,
    ) -> Result<NativeResult, InterpError> {
        use id::*;
        let result = match self.id {
            ARR_PUSH => arr_push(&this, args),
            ARR_POP => arr_pop(&this),
            ARR_SHIFT => arr_shift(&this),
            ARR_JOIN => arr_join(&this, args),
            ARR_SLICE => arr_slice(&this, args),
            ARR_CONCAT => arr_concat(&this, args),
            ARR_INDEX_OF => arr_index_of(&this, args),
            ARR_INCLUDES => Value::boolean(arr_find(&this, &args).is_some()),
            ARR_REVERSE => arr_reverse(&this),
            // Callback methods — drive the JS callback via call_value.
            ARR_MAP => return arr_map(interp, module, &this, args),
            ARR_FILTER => return arr_filter(interp, module, &this, args),
            ARR_FOR_EACH => return arr_for_each(interp, module, &this, args),
            ARR_REDUCE => return arr_reduce(interp, module, &this, args),
            ARR_FIND => return arr_find_cb(interp, module, &this, args),
            ARR_SOME => return arr_some_every(interp, module, &this, args, false),
            ARR_EVERY => return arr_some_every(interp, module, &this, args, true),
            ARR_SORT => return arr_sort(interp, module, &this, args),
            ARR_FLAT => arr_flat(&this, args),
            ARR_FILL => arr_fill(&this, args),
            ARR_AT => arr_at(&this, args),
            STR_CHAR_AT => str_char_at(&this, args),
            STR_CHAR_CODE_AT => str_char_code_at(&this, args),
            STR_SLICE => str_slice(&this, args),
            STR_SUBSTRING => str_substring(&this, args),
            STR_INDEX_OF => Value::integer(str_index_of(&this, args) as i32),
            STR_INCLUDES => Value::boolean(str_index_of(&this, args) >= 0),
            STR_UPPER => str_map(&this, |s| s.to_uppercase()),
            STR_LOWER => str_map(&this, |s| s.to_lowercase()),
            STR_TRIM => str_map(&this, |s| s.trim().to_string()),
            STR_REPEAT => str_repeat(&this, args),
            STR_SPLIT => str_split(&this, args),
            STR_CONCAT => str_concat(&this, args),
            STR_PAD_START => str_pad(&this, args, true),
            STR_PAD_END => str_pad(&this, args, false),
            STR_STARTS_WITH => str_starts_ends(&this, args, true),
            STR_ENDS_WITH => str_starts_ends(&this, args, false),
            STR_REPLACE => return str_replace(interp, module, &this, args),
            STR_AT => str_at(&this, args),
            // ---- globals ----
            CONSOLE_LOG | CONSOLE_ERROR | CONSOLE_WARN => {
                let parts: Vec<String> = args.iter().map(to_string).collect();
                println!("{}", parts.join(" "));
                Value::undefined()
            }
            MATH_MAX => math_min_max(args, false),
            MATH_MIN => math_min_max(args, true),
            MATH_ABS => Value::number(arg_f64(&args, 0).unwrap_or(f64::NAN).abs()),
            MATH_FLOOR => Value::number(arg_f64(&args, 0).unwrap_or(f64::NAN).floor()),
            MATH_CEIL => Value::number(arg_f64(&args, 0).unwrap_or(f64::NAN).ceil()),
            MATH_ROUND => Value::number(math_round(arg_f64(&args, 0).unwrap_or(f64::NAN))),
            MATH_SQRT => Value::number(arg_f64(&args, 0).unwrap_or(f64::NAN).sqrt()),
            MATH_POW => Value::number(
                arg_f64(&args, 0)
                    .unwrap_or(0.0)
                    .powf(arg_f64(&args, 1).unwrap_or(0.0)),
            ),
            MATH_SIGN => math_sign(arg_f64(&args, 0).unwrap_or(f64::NAN)),
            OBJECT_KEYS | OBJECT_VALUES | OBJECT_ENTRIES | OBJECT_ASSIGN => {
                if let Some(target) = args.first() {
                    interp.ensure_deferred_namespace(module, target)?;
                    if matches!(self.id, OBJECT_KEYS | OBJECT_VALUES | OBJECT_ENTRIES) {
                        validate_namespace_bindings(target)?;
                    }
                }
                match self.id {
                    OBJECT_KEYS => object_keys(&args),
                    OBJECT_VALUES => object_values(&args),
                    OBJECT_ENTRIES => object_entries(&args),
                    OBJECT_ASSIGN => object_assign(args),
                    _ => unreachable!(),
                }
            }
            OBJECT_CREATE => return object_create(&args),
            JSON_STRINGIFY => json_stringify(&args),
            PARSE_INT => parse_int(&args),
            PARSE_FLOAT => parse_float(&args),
            IS_NAN => Value::boolean(arg_f64(&args, 0).unwrap_or(f64::NAN).is_nan()),
            IS_FINITE => Value::boolean(arg_f64(&args, 0).map(|n| n.is_finite()).unwrap_or(false)),
            NUMBER_FN => to_number(&args),
            STRING_FN => Value::string(args.get(0).map(to_string).unwrap_or_default()),
            BOOLEAN_FN => Value::boolean(args.get(0).map(|v| is_truthy(v)).unwrap_or(false)),
            ARRAY_CTOR => crate::interp::make_array(args),
            MAP_CTOR => collection_object("Map"),
            SET_CTOR => collection_object("Set"),
            WRAPPER_VALUE_OF => {
                crate::interp::get_property(&this, &Value::string("[[PrimitiveValue]]"))
            }
            SYMBOL_FN => Value::symbol(js_runtime::value::JsSymbol::new(
                args.first().map(to_string),
            )),
            SYMBOL_TO_STRING => Value::string(to_string(&this)),
            FUNCTION_CALL => {
                let receiver = args.first().cloned().unwrap_or_else(Value::undefined);
                return interp
                    .call_value(module, this, args.into_iter().skip(1).collect(), receiver)
                    .map(NativeResult::Value);
            }
            FUNCTION_BIND => return bind_function(&this, args),
            EVAL => {
                let value = args.into_iter().next().unwrap_or_else(Value::undefined);
                return interp
                    .eval_value(module, value, false)
                    .map(NativeResult::Value);
            }
            OBJECT_HAS_OWN | OBJECT_PROP_ENUM => {
                return object_prototype_query(&this, &args, self.id == OBJECT_PROP_ENUM)
            }
            OBJECT_GET_PROTO => object_get_prototype(&args),
            OBJECT_SET_PROTO => return object_set_prototype(&args),
            OBJECT_IS_EXTENSIBLE => Value::boolean(object_is_extensible(&args)),
            OBJECT_PREVENT_EXTENSIONS => return object_prevent_extensions(&args, false),
            OBJECT_GET_OWN_DESC => return object_get_own_descriptor(&args),
            OBJECT_DEFINE_PROP => return object_define_property(&args, false),
            OBJECT_GET_OWN_NAMES => object_own_keys(&args, OwnKeyKind::Strings),
            OBJECT_GET_OWN_SYMBOLS => object_own_keys(&args, OwnKeyKind::Symbols),
            OBJECT_FREEZE => return object_freeze(&args),
            OBJECT_IS_FROZEN => Value::boolean(object_is_frozen(&args)),
            REFLECT_DEFINE_PROP => return object_define_property(&args, true),
            REFLECT_DELETE_PROP => Value::boolean(reflect_delete_property(&args)),
            REFLECT_GET => {
                let target = args.first().cloned().unwrap_or_else(Value::undefined);
                let key = args.get(1).cloned().unwrap_or_else(Value::undefined);
                let receiver = args.get(2).cloned().unwrap_or_else(|| target.clone());
                return interp
                    .get_property_value(module, &target, &key, &receiver)
                    .map(NativeResult::Value);
            }
            REFLECT_HAS => Value::boolean(reflect_has(&args)),
            REFLECT_PREVENT_EXTENSIONS => return object_prevent_extensions(&args, true),
            REFLECT_SET => Value::boolean(reflect_set(&args)),
            REFLECT_OWN_KEYS => object_own_keys(&args, OwnKeyKind::All),
            ARRAY_IS_ARRAY => Value::boolean(
                matches!(args.get(0).map(|v| v.data().clone()), Some(ValueData::Object(o)) if o.borrow().is_exotic_array),
            ),
            ERROR_CTOR => return Ok(NativeResult::Value(error_ctor(&this, &args, "Error"))),
            TYPE_ERROR_CTOR => {
                return Ok(NativeResult::Value(error_ctor(&this, &args, "TypeError")))
            }
            RANGE_ERROR_CTOR => {
                return Ok(NativeResult::Value(error_ctor(&this, &args, "RangeError")))
            }
            SYNTAX_ERROR_CTOR => {
                return Ok(NativeResult::Value(error_ctor(&this, &args, "SyntaxError")))
            }
            REF_ERROR_CTOR => {
                return Ok(NativeResult::Value(error_ctor(
                    &this,
                    &args,
                    "ReferenceError",
                )))
            }
            JSON_PARSE => return json_parse(&args),
            NUM_TO_FIXED => return num_to_fixed(&this, &args),
            NUM_TO_STRING => return num_to_string(&this, &args),
            NUM_IS_INTEGER => Value::boolean(num_is_integer(&args)),
            NUM_IS_FINITE => {
                Value::boolean(arg_f64(&args, 0).map(|n| n.is_finite()).unwrap_or(false))
            }
            NUM_IS_NAN => Value::boolean(arg_f64(&args, 0).map(|n| n.is_nan()).unwrap_or(true)),
            NUM_PARSE_INT => parse_int(&args),
            NUM_PARSE_FLOAT => parse_float(&args),
            STR_TRIM_START => str_map(&this, |s| s.trim_start().to_string()),
            STR_TRIM_END => str_map(&this, |s| s.trim_end().to_string()),
            STR_FROM_CHAR_CODE => return str_from_char_code(&args),
            STR_CODE_POINT_AT => return str_code_point_at(&this, &args),
            REGEX_TEST => regex_test(&this, &args),
            REGEX_EXEC => return regex_exec(&this, &args),
            STR_MATCH => return str_match(&this, &args),
            STR_SEARCH => Value::integer(str_search(&this, &args) as i32),
            // ---- test262 harness ----
            TEST262_ERROR_CTOR => {
                return Ok(NativeResult::Value(error_ctor(
                    &this,
                    &args,
                    "Test262Error",
                )))
            }
            ASSERT_SAME_VALUE => return assert_same_value(&args, false),
            ASSERT_NOT_SAME_VALUE => return assert_same_value(&args, true),
            ASSERT_THROWS => return assert_throws(interp, module, &args),
            ASSERT => return assert_truthy(&args),
            DONE => return done(interp, &args),
            PROMISE_CTOR => return promise_constructor(interp, module, &this, args),
            PROMISE_RESOLVE => {
                return Ok(NativeResult::Value(promise_resolve(interp, module, args)?))
            }
            PROMISE_REJECT => {
                return Ok(NativeResult::Value(promise_rejected(
                    args.into_iter().next().unwrap_or_else(Value::undefined),
                )))
            }
            PROMISE_THEN => return promise_then(interp, &this, args),
            PROMISE_CATCH => {
                let on_rejected = args.into_iter().next().unwrap_or_else(Value::undefined);
                return promise_then(interp, &this, vec![Value::undefined(), on_rejected]);
            }
            PROMISE_RESOLVING_FULFILL | PROMISE_RESOLVING_REJECT => {
                let promise = _f.bound_object.clone().ok_or_else(|| {
                    InterpError::Internal("Promise resolving function lost its promise".into())
                })?;
                let value = args.into_iter().next().unwrap_or_else(Value::undefined);
                if self.id == PROMISE_RESOLVING_REJECT {
                    reject_promise(interp, promise, value);
                } else {
                    resolve_promise(interp, module, promise, value)?;
                }
                return Ok(NativeResult::Value(Value::undefined()));
            }
            _ => Value::undefined(),
        };
        Ok(NativeResult::Value(result))
    }
}

pub(crate) enum AwaitedPromise {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

fn promise_object(value: &Value) -> Option<JsObject> {
    match value.data() {
        ValueData::Object(object) if object.borrow().promise.is_some() => Some(object.clone()),
        _ => None,
    }
}

fn new_promise() -> JsObject {
    let promise = js_runtime::object::ObjectData::promise();
    {
        let mut object = promise.borrow_mut();
        object.proto = Some(promise_prototype());
        object
            .constructor_chain
            .push(js_runtime::object::ConstructorIdentity {
                module_index: 0,
                function_id: 0,
                native_id: Some(id::PROMISE_CTOR),
            });
    }
    promise
}

fn promise_prototype() -> Value {
    thread_local! {
        static PROTOTYPE: JsObject = js_runtime::object::ObjectData::new_handle();
    }
    PROTOTYPE.with(|prototype| Value::object(prototype.clone()))
}

pub(crate) fn promise_pending() -> JsObject {
    new_promise()
}

pub(crate) fn promise_rejected(value: Value) -> Value {
    let promise = new_promise();
    promise.borrow_mut().promise.as_mut().unwrap().state = PromiseState::Rejected(value);
    Value::object(promise)
}

pub(crate) fn promise_result(value: &Value) -> Option<AwaitedPromise> {
    let promise = promise_object(value)?;
    let state = promise.borrow().promise.as_ref().unwrap().state.clone();
    Some(match state {
        PromiseState::Pending => AwaitedPromise::Pending,
        PromiseState::Fulfilled(value) => AwaitedPromise::Fulfilled(value),
        PromiseState::Rejected(value) => AwaitedPromise::Rejected(value),
    })
}

fn settle_promise(interp: &mut Interpreter, promise: JsObject, rejected: bool, value: Value) {
    let reactions = {
        let mut object = promise.borrow_mut();
        let data = object.promise.as_mut().expect("Promise object data");
        if !matches!(data.state, PromiseState::Pending) {
            return;
        }
        data.state = if rejected {
            PromiseState::Rejected(value.clone())
        } else {
            PromiseState::Fulfilled(value.clone())
        };
        std::mem::take(&mut data.reactions)
    };
    for reaction in reactions {
        interp.enqueue_promise_job(crate::interp::PromiseJob::Reaction {
            reaction,
            argument: value.clone(),
            rejected,
        });
    }
}

pub(crate) fn fulfill_promise(interp: &mut Interpreter, promise: JsObject, value: Value) {
    settle_promise(interp, promise, false, value);
}

pub(crate) fn reject_promise(interp: &mut Interpreter, promise: JsObject, reason: Value) {
    settle_promise(interp, promise, true, reason);
}

pub(crate) fn resolve_promise(
    interp: &mut Interpreter,
    modules: &BytecodeGraph<'_>,
    promise: JsObject,
    value: Value,
) -> Result<(), InterpError> {
    if matches!(value.data(), ValueData::Object(object) if std::rc::Rc::ptr_eq(object, &promise)) {
        reject_promise(
            interp,
            promise,
            type_error_value("a Promise cannot resolve to itself"),
        );
        return Ok(());
    }

    if let Some(source) = promise_object(&value) {
        let reaction = PromiseReaction {
            on_fulfilled: None,
            on_rejected: None,
            result: promise,
        };
        let state = {
            let mut source = source.borrow_mut();
            let data = source.promise.as_mut().unwrap();
            match &data.state {
                PromiseState::Pending => {
                    data.reactions.push(reaction.clone());
                    None
                }
                state => Some(state.clone()),
            }
        };
        if let Some(state) = state {
            let (argument, rejected) = match state {
                PromiseState::Fulfilled(value) => (value, false),
                PromiseState::Rejected(value) => (value, true),
                PromiseState::Pending => unreachable!(),
            };
            interp.enqueue_promise_job(crate::interp::PromiseJob::Reaction {
                reaction,
                argument,
                rejected,
            });
        }
        return Ok(());
    }

    if value.is_object() {
        let then = crate::interp::get_property(&value, &Value::string("then"));
        if then.is_function() {
            interp.enqueue_promise_job(crate::interp::PromiseJob::ResolveThenable {
                promise,
                thenable: value,
                then,
            });
            return Ok(());
        }
    }

    settle_promise(interp, promise, false, value);
    let _ = modules;
    Ok(())
}

pub(crate) fn promise_resolved(
    interp: &mut Interpreter,
    modules: &BytecodeGraph<'_>,
    value: Value,
) -> Result<Value, InterpError> {
    if promise_object(&value).is_some() {
        return Ok(value);
    }
    let promise = new_promise();
    resolve_promise(interp, modules, promise.clone(), value)?;
    Ok(Value::object(promise))
}

fn promise_resolve(
    interp: &mut Interpreter,
    modules: &BytecodeGraph<'_>,
    args: Vec<Value>,
) -> Result<Value, InterpError> {
    let value = args.into_iter().next().unwrap_or_else(Value::undefined);
    if promise_object(&value).is_some() {
        Ok(value)
    } else {
        promise_resolved(interp, modules, value)
    }
}

fn promise_constructor(
    interp: &mut Interpreter,
    modules: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let Some(object) = (match this.data() {
        ValueData::Object(object) => Some(object.clone()),
        _ => None,
    }) else {
        return Err(InterpError::Throw(type_error_value(
            "Promise constructor requires new",
        )));
    };
    {
        let mut object_data = object.borrow_mut();
        object_data.class = "Promise";
        object_data.promise = Some(PromiseData {
            state: PromiseState::Pending,
            reactions: Vec::new(),
        });
    }
    let executor = args.into_iter().next().unwrap_or_else(Value::undefined);
    if !executor.is_function() {
        return Err(InterpError::Throw(type_error_value(
            "Promise executor is not callable",
        )));
    }
    let (fulfill, reject) = resolving_functions(&object);
    if let Err(error) =
        interp.call_value(modules, executor, vec![fulfill, reject], Value::undefined())
    {
        match error {
            InterpError::Throw(reason) => reject_promise(interp, object.clone(), reason),
            error => return Err(error),
        }
    }
    Ok(NativeResult::Value(Value::object(object)))
}

fn promise_then(
    interp: &mut Interpreter,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let promise = promise_object(this).ok_or_else(|| {
        InterpError::Throw(type_error_value(
            "Promise.prototype.then receiver is not a Promise",
        ))
    })?;
    let callable = |value: Option<&Value>| value.filter(|value| value.is_function()).cloned();
    let reaction = PromiseReaction {
        on_fulfilled: callable(args.first()),
        on_rejected: callable(args.get(1)),
        result: new_promise(),
    };
    let state = {
        let mut object = promise.borrow_mut();
        let data = object.promise.as_mut().unwrap();
        match &data.state {
            PromiseState::Pending => {
                data.reactions.push(reaction.clone());
                None
            }
            state => Some(state.clone()),
        }
    };
    if let Some(state) = state {
        let (argument, rejected) = match state {
            PromiseState::Fulfilled(value) => (value, false),
            PromiseState::Rejected(value) => (value, true),
            PromiseState::Pending => unreachable!(),
        };
        interp.enqueue_promise_job(crate::interp::PromiseJob::Reaction {
            reaction: reaction.clone(),
            argument,
            rejected,
        });
    }
    Ok(NativeResult::Value(Value::object(reaction.result)))
}

pub(crate) fn resolving_functions(promise: &JsObject) -> (Value, Value) {
    let resolving = |name: &str, id: u16| {
        let mut function = JsFunction::new(name, 0, 1);
        function.native = Some(id);
        function.bound_object = Some(promise.clone());
        Value::function(function)
    };
    (
        resolving("resolve", id::PROMISE_RESOLVING_FULFILL),
        resolving("reject", id::PROMISE_RESOLVING_REJECT),
    )
}

fn type_error_value(message: &str) -> Value {
    error_ctor(&Value::undefined(), &[Value::string(message)], "TypeError")
}

fn collection_object(class: &'static str) -> Value {
    let object = js_runtime::object::ObjectData::new_handle();
    object.borrow_mut().class = class;
    crate::interp::set_property(
        &Value::object(object.clone()),
        &Value::string("size"),
        Value::integer(0),
    );
    Value::object(object)
}

pub(crate) fn construct_builtin(callee: &Value, args: &[Value]) -> Option<Value> {
    use id::*;
    let native = callee.as_function()?.native?;
    let (class, primitive) = match native {
        NUMBER_FN => ("Number", Some(to_number(args))),
        STRING_FN => (
            "String",
            Some(Value::string(
                args.first().map(to_string).unwrap_or_default(),
            )),
        ),
        BOOLEAN_FN => (
            "Boolean",
            Some(Value::boolean(args.first().map(is_truthy).unwrap_or(false))),
        ),
        ARRAY_CTOR => return Some(crate::interp::make_array(args.to_vec())),
        MAP_CTOR => return Some(collection_object("Map")),
        SET_CTOR => return Some(collection_object("Set")),
        _ => return None,
    };
    let object = js_runtime::object::ObjectData::new_handle();
    object.borrow_mut().class = class;
    let value = Value::object(object);
    crate::interp::set_property(
        &value,
        &Value::string("[[PrimitiveValue]]"),
        primitive.unwrap(),
    );
    Some(value)
}

fn validate_namespace_bindings(value: &Value) -> Result<(), InterpError> {
    let ValueData::Object(object) = value.data() else {
        return Ok(());
    };
    let object = object.borrow();
    let Some(namespace) = &object.module_namespace else {
        return Ok(());
    };
    if namespace.values().any(|binding| !binding.is_initialized()) {
        return Err(InterpError::Throw(error_ctor(
            &Value::undefined(),
            &[Value::string("cannot access binding before initialization")],
            "ReferenceError",
        )));
    }
    Ok(())
}

enum OwnKeyKind {
    Strings,
    Symbols,
    All,
}

fn own_descriptor(target: &Value, key: &Value) -> Result<Option<PropertyDescriptor>, InterpError> {
    let ValueData::Object(object) = target.data() else {
        return Ok(None);
    };
    let data = object.borrow();
    match key.data() {
        ValueData::Symbol(symbol) => Ok(data.symbol_properties.get(&symbol.id).cloned()),
        _ => {
            let name = crate::interp::prop_name(key);
            if let Some(namespace) = &data.module_namespace {
                let Some(binding) = namespace.get(&name) else {
                    return Ok(None);
                };
                let value = binding.get().map_err(|_| {
                    InterpError::Throw(error_ctor(
                        &Value::undefined(),
                        &[Value::string("cannot access binding before initialization")],
                        "ReferenceError",
                    ))
                })?;
                return Ok(Some(PropertyDescriptor::Data {
                    value,
                    attr: Attribute {
                        writable: true,
                        enumerable: true,
                        configurable: false,
                    },
                }));
            }
            Ok(data.properties.get(&name).cloned())
        }
    }
}

fn descriptor_value(descriptor: PropertyDescriptor) -> Value {
    let object = js_runtime::object::ObjectData::new_handle();
    let value = Value::object(object);
    match descriptor {
        PropertyDescriptor::Data { value: data, attr } => {
            crate::interp::set_property(&value, &Value::string("value"), data);
            crate::interp::set_property(
                &value,
                &Value::string("writable"),
                Value::boolean(attr.writable),
            );
            crate::interp::set_property(
                &value,
                &Value::string("enumerable"),
                Value::boolean(attr.enumerable),
            );
            crate::interp::set_property(
                &value,
                &Value::string("configurable"),
                Value::boolean(attr.configurable),
            );
        }
        PropertyDescriptor::Accessor { get, set, attr } => {
            crate::interp::set_property(
                &value,
                &Value::string("get"),
                get.unwrap_or_else(Value::undefined),
            );
            crate::interp::set_property(
                &value,
                &Value::string("set"),
                set.unwrap_or_else(Value::undefined),
            );
            crate::interp::set_property(
                &value,
                &Value::string("enumerable"),
                Value::boolean(attr.enumerable),
            );
            crate::interp::set_property(
                &value,
                &Value::string("configurable"),
                Value::boolean(attr.configurable),
            );
        }
    }
    value
}

fn object_prototype_query(
    this: &Value,
    args: &[Value],
    enumerable: bool,
) -> Result<NativeResult, InterpError> {
    let key = args.first().cloned().unwrap_or_else(Value::undefined);
    let descriptor = own_descriptor(this, &key)?;
    let result = if enumerable {
        descriptor.is_some_and(|descriptor| match descriptor {
            PropertyDescriptor::Data { attr, .. } | PropertyDescriptor::Accessor { attr, .. } => {
                attr.enumerable
            }
        })
    } else {
        descriptor.is_some()
    };
    Ok(NativeResult::Value(Value::boolean(result)))
}

fn object_get_prototype(args: &[Value]) -> Value {
    match args.first().map(Value::data) {
        Some(ValueData::Object(object)) => {
            object.borrow().proto.clone().unwrap_or_else(Value::null)
        }
        _ => Value::null(),
    }
}

fn object_create(args: &[Value]) -> Result<NativeResult, InterpError> {
    let prototype = args.first().cloned().unwrap_or_else(Value::undefined);
    if !matches!(prototype.data(), ValueData::Null) && !prototype.is_object() {
        return Err(InterpError::Throw(type_error_value(
            "Object prototype may only be an object or null",
        )));
    }
    let object = js_runtime::object::ObjectData::new_handle();
    {
        let mut data = object.borrow_mut();
        if matches!(prototype.data(), ValueData::Null) {
            data.explicit_null_prototype = true;
        } else {
            data.proto = Some(prototype);
        }
    }
    Ok(NativeResult::Value(Value::object(object)))
}

fn object_set_prototype(args: &[Value]) -> Result<NativeResult, InterpError> {
    let target = args.first().cloned().unwrap_or_else(Value::undefined);
    let prototype = args.get(1).cloned().unwrap_or_else(Value::undefined);
    let ValueData::Object(object) = target.data() else {
        return Err(InterpError::Throw(type_error_value(
            "target is not an object",
        )));
    };
    let same = match (&object.borrow().proto, prototype.data()) {
        (None, ValueData::Null) => true,
        (Some(current), _) => value_eq_strict(current, &prototype),
        _ => false,
    };
    if same {
        return Ok(NativeResult::Value(target));
    }
    if object.borrow().module_namespace.is_some() || object.borrow().non_extensible {
        return Err(InterpError::Throw(type_error_value(
            "object prototype cannot be changed",
        )));
    }
    if !prototype.is_null() && !prototype.is_object() {
        return Err(InterpError::Throw(type_error_value(
            "prototype must be an object or null",
        )));
    }
    object.borrow_mut().proto = (!prototype.is_null()).then_some(prototype);
    Ok(NativeResult::Value(target))
}

fn object_is_extensible(args: &[Value]) -> bool {
    matches!(args.first().map(Value::data), Some(ValueData::Object(object)) if !object.borrow().non_extensible)
}

fn object_prevent_extensions(args: &[Value], reflect: bool) -> Result<NativeResult, InterpError> {
    let target = args.first().cloned().unwrap_or_else(Value::undefined);
    let ValueData::Object(object) = target.data() else {
        return if reflect {
            Ok(NativeResult::Value(Value::boolean(false)))
        } else {
            Err(InterpError::Throw(type_error_value(
                "target is not an object",
            )))
        };
    };
    object.borrow_mut().non_extensible = true;
    Ok(NativeResult::Value(if reflect {
        Value::boolean(true)
    } else {
        target
    }))
}

fn object_get_own_descriptor(args: &[Value]) -> Result<NativeResult, InterpError> {
    let target = args.first().cloned().unwrap_or_else(Value::undefined);
    let key = args.get(1).cloned().unwrap_or_else(Value::undefined);
    Ok(NativeResult::Value(
        own_descriptor(&target, &key)?
            .map(descriptor_value)
            .unwrap_or_else(Value::undefined),
    ))
}

fn requested_field(descriptor: &Value, name: &str) -> Option<Value> {
    let ValueData::Object(object) = descriptor.data() else {
        return None;
    };
    object
        .borrow()
        .properties
        .get(name)
        .map(|property| match property {
            PropertyDescriptor::Data { value, .. } => value.clone(),
            PropertyDescriptor::Accessor { .. } => Value::undefined(),
        })
}

fn object_define_property(args: &[Value], reflect: bool) -> Result<NativeResult, InterpError> {
    let target = args.first().cloned().unwrap_or_else(Value::undefined);
    let key = args.get(1).cloned().unwrap_or_else(Value::undefined);
    let requested = args.get(2).cloned().unwrap_or_else(Value::undefined);
    let current = own_descriptor(&target, &key)?;
    let compatible = current.as_ref().is_some_and(|current| {
        let (value, attr) = match current {
            PropertyDescriptor::Data { value, attr } => (value, attr),
            PropertyDescriptor::Accessor { .. } => return false,
        };
        requested_field(&requested, "value").is_none_or(|new| same_value(&new, value))
            && requested_field(&requested, "writable")
                .is_none_or(|new| is_truthy(&new) == attr.writable)
            && requested_field(&requested, "enumerable")
                .is_none_or(|new| is_truthy(&new) == attr.enumerable)
            && requested_field(&requested, "configurable")
                .is_none_or(|new| is_truthy(&new) == attr.configurable)
            && requested_field(&requested, "get").is_none()
            && requested_field(&requested, "set").is_none()
    });
    if compatible {
        return Ok(NativeResult::Value(if reflect {
            Value::boolean(true)
        } else {
            target
        }));
    }
    if reflect {
        Ok(NativeResult::Value(Value::boolean(false)))
    } else {
        Err(InterpError::Throw(type_error_value(
            "property cannot be defined",
        )))
    }
}

fn object_own_keys(args: &[Value], kind: OwnKeyKind) -> Value {
    let Some(ValueData::Object(object)) = args.first().map(Value::data) else {
        return crate::interp::make_array(Vec::new());
    };
    let object = object.borrow();
    let mut keys = Vec::new();
    if matches!(kind, OwnKeyKind::Strings | OwnKeyKind::All) {
        if let Some(namespace) = &object.module_namespace {
            keys.extend(namespace.keys().cloned().map(Value::string));
        } else {
            let mut names: Vec<_> = object.properties.keys().cloned().collect();
            names.sort();
            keys.extend(names.into_iter().map(Value::string));
        }
    }
    if matches!(kind, OwnKeyKind::Symbols | OwnKeyKind::All) {
        let mut symbols: Vec<_> = object.symbol_properties.keys().copied().collect();
        symbols.sort_unstable();
        keys.extend(symbols.into_iter().map(|id| {
            let symbol = if id == js_runtime::value::JsSymbol::to_string_tag().id {
                js_runtime::value::JsSymbol::to_string_tag()
            } else if id == js_runtime::value::JsSymbol::iterator().id {
                js_runtime::value::JsSymbol::iterator()
            } else if id == js_runtime::value::JsSymbol::async_iterator().id {
                js_runtime::value::JsSymbol::async_iterator()
            } else {
                js_runtime::value::JsSymbol {
                    id,
                    description: None,
                }
            };
            Value::symbol(symbol)
        }));
    }
    crate::interp::make_array(keys)
}

fn object_freeze(args: &[Value]) -> Result<NativeResult, InterpError> {
    let target = args.first().cloned().unwrap_or_else(Value::undefined);
    if matches!(target.data(), ValueData::Object(object) if object.borrow().module_namespace.as_ref().is_some_and(|namespace| !namespace.is_empty()))
    {
        return Err(InterpError::Throw(type_error_value(
            "module namespace exports cannot be made non-writable",
        )));
    }
    if let ValueData::Object(object) = target.data() {
        object.borrow_mut().non_extensible = true;
    }
    Ok(NativeResult::Value(target))
}

fn object_is_frozen(args: &[Value]) -> bool {
    matches!(args.first().map(Value::data), Some(ValueData::Object(object)) if object.borrow().non_extensible && object.borrow().module_namespace.as_ref().is_none_or(|namespace| namespace.is_empty()))
}

fn reflect_delete_property(args: &[Value]) -> bool {
    let Some(target) = args.first() else {
        return false;
    };
    let key = args.get(1).cloned().unwrap_or_else(Value::undefined);
    crate::interp::delete_property(target, &key)
}

fn reflect_has(args: &[Value]) -> bool {
    let Some(target) = args.first() else {
        return false;
    };
    let key = args.get(1).cloned().unwrap_or_else(Value::undefined);
    crate::interp::has_property(target, &key)
}

fn reflect_set(args: &[Value]) -> bool {
    let Some(target) = args.first() else {
        return false;
    };
    let key = args.get(1).cloned().unwrap_or_else(Value::undefined);
    let value = args.get(2).cloned().unwrap_or_else(Value::undefined);
    crate::interp::set_property_checked(target, &key, value)
}

// ---- helpers --------------------------------------------------------------

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).map(to_string).unwrap_or_default()
}

fn arg_int(args: &[Value], i: usize) -> Option<i32> {
    args.get(i).and_then(|v| match v.data() {
        ValueData::Integer(n) => Some(*n),
        ValueData::Number(n) => Some(*n as i32),
        _ => None,
    })
}

fn arg_f64(args: &[Value], i: usize) -> Option<f64> {
    args.get(i).and_then(|v| match v.data() {
        ValueData::Integer(n) => Some(*n as f64),
        ValueData::Number(n) => Some(*n),
        ValueData::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
        ValueData::String(s) => s.trim().parse::<f64>().ok(),
        ValueData::Null => Some(0.0),
        _ => None,
    })
}

/// Strict-equality used by indexOf/includes.
fn value_eq_strict(a: &Value, b: &Value) -> bool {
    match (a.data(), b.data()) {
        (ValueData::Integer(x), ValueData::Integer(y)) => x == y,
        (ValueData::Number(x), ValueData::Number(y)) => x == y,
        (ValueData::Integer(x), ValueData::Number(y)) => (*x as f64) == *y,
        (ValueData::Number(x), ValueData::Integer(y)) => *x == (*y as f64),
        (ValueData::String(x), ValueData::String(y)) => x == y,
        (ValueData::Boolean(x), ValueData::Boolean(y)) => x == y,
        (ValueData::Symbol(x), ValueData::Symbol(y)) => x.id == y.id,
        (ValueData::Object(x), ValueData::Object(y)) => std::rc::Rc::ptr_eq(x, y),
        (ValueData::Function(x), ValueData::Function(y)) => {
            std::rc::Rc::ptr_eq(&x.object, &y.object)
        }
        (ValueData::Undefined, ValueData::Undefined) | (ValueData::Null, ValueData::Null) => true,
        _ => false,
    }
}

fn arr_find(this: &Value, args: &[Value]) -> Option<usize> {
    let target = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    (0..len).find(|&i| value_eq_strict(&array_get(this, i), &target))
}

// ---- Array methods --------------------------------------------------------

fn arr_push(this: &Value, args: Vec<Value>) -> Value {
    for a in args {
        crate::interp::array_append(this, a);
    }
    Value::integer(array_len(this) as i32)
}

fn arr_pop(this: &Value) -> Value {
    let len = array_len(this);
    if len == 0 {
        return Value::undefined();
    }
    let last = array_get(this, len - 1);
    if let ValueData::Object(o) = this.data() {
        let mut b = o.borrow_mut();
        b.properties.remove(&(len - 1).to_string());
        b.properties.insert(
            "length".to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::integer((len - 1) as i32)),
        );
    }
    last
}

fn arr_shift(this: &Value) -> Value {
    let len = array_len(this);
    if len == 0 {
        return Value::undefined();
    }
    let first = array_get(this, 0);
    if let ValueData::Object(o) = this.data() {
        let mut b = o.borrow_mut();
        // Re-index: i -> i-1.
        let mut new_props = std::collections::HashMap::new();
        let keys: Vec<String> = b.properties.keys().cloned().collect();
        for k in keys {
            if let Ok(i) = k.parse::<usize>() {
                if let Some(d) = b.properties.remove(&k) {
                    if i > 0 {
                        new_props.insert((i - 1).to_string(), d);
                    }
                }
            } else {
                let d = b.properties.remove(&k).unwrap();
                new_props.insert(k, d);
            }
        }
        b.properties = new_props;
        b.properties.insert(
            "length".to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::integer((len - 1) as i32)),
        );
    }
    first
}

fn arr_join(this: &Value, args: Vec<Value>) -> Value {
    let sep = if args.is_empty() || args[0].is_undefined() {
        ",".to_string()
    } else {
        to_string(&args[0])
    };
    let len = array_len(this);
    let parts: Vec<String> = (0..len)
        .map(|i| {
            let v = array_get(this, i);
            if v.is_null() || v.is_undefined() {
                String::new()
            } else {
                to_string(&v)
            }
        })
        .collect();
    Value::string(parts.join(&sep))
}

fn arr_slice(this: &Value, args: Vec<Value>) -> Value {
    let len = array_len(this) as isize;
    let start = clamp_index(arg_int(&args, 0).unwrap_or(0) as isize, len);
    let end = match arg_int(&args, 1) {
        Some(e) => clamp_index(e as isize, len),
        None => len,
    };
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        out.push(array_get(this, i as usize));
        i += 1;
    }
    make_array(out)
}

fn arr_concat(this: &Value, args: Vec<Value>) -> Value {
    let mut out: Vec<Value> = (0..array_len(this)).map(|i| array_get(this, i)).collect();
    for a in args {
        if matches!(a.data(), ValueData::Object(o) if o.borrow().is_exotic_array) {
            for i in 0..array_len(&a) {
                out.push(array_get(&a, i));
            }
        } else {
            out.push(a);
        }
    }
    make_array(out)
}

fn arr_index_of(this: &Value, args: Vec<Value>) -> Value {
    Value::integer(arr_find(this, &args).map(|i| i as i32).unwrap_or(-1))
}

fn arr_reverse(this: &Value) -> Value {
    let len = array_len(this);
    let vals: Vec<Value> = (0..len).map(|i| array_get(this, i)).collect();
    if let ValueData::Object(o) = this.data() {
        let mut b = o.borrow_mut();
        for (i, v) in vals.into_iter().enumerate() {
            b.properties.insert(
                (len - 1 - i).to_string(),
                js_runtime::object::PropertyDescriptor::data(v),
            );
        }
    }
    this.clone()
}

fn clamp_index(i: isize, len: isize) -> isize {
    if i < 0 {
        (len + i).max(0)
    } else {
        i.min(len)
    }
}

// ---- String methods -------------------------------------------------------

fn str_map(this: &Value, f: impl Fn(&str) -> String) -> Value {
    match this.data() {
        ValueData::String(s) => Value::string(f(s.as_str())),
        _ => Value::undefined(),
    }
}

fn str_char_at(this: &Value, args: Vec<Value>) -> Value {
    let i = arg_int(&args, 0).unwrap_or(0);
    if let ValueData::String(s) = this.data() {
        if let Some(c) = s.as_str().chars().nth(i as usize) {
            return Value::string(c.to_string());
        }
    }
    Value::string(String::new())
}

fn str_char_code_at(this: &Value, args: Vec<Value>) -> Value {
    let i = arg_int(&args, 0).unwrap_or(0);
    if let ValueData::String(s) = this.data() {
        if let Some(c) = s.as_str().chars().nth(i as usize) {
            return Value::integer(c as i32);
        }
    }
    Value::number(f64::NAN)
}

fn str_slice(this: &Value, args: Vec<Value>) -> Value {
    if let ValueData::String(s) = this.data() {
        let chars: Vec<char> = s.as_str().chars().collect();
        let len = chars.len() as isize;
        let start = clamp_index(arg_int(&args, 0).unwrap_or(0) as isize, len) as usize;
        let end = match arg_int(&args, 1) {
            Some(e) => clamp_index(e as isize, len) as usize,
            None => chars.len(),
        };
        return Value::string(chars[start..end].iter().collect::<String>());
    }
    Value::undefined()
}

fn str_substring(this: &Value, args: Vec<Value>) -> Value {
    if let ValueData::String(s) = this.data() {
        let chars: Vec<char> = s.as_str().chars().collect();
        let len = chars.len() as isize;
        let mut start = arg_int(&args, 0).unwrap_or(0).max(0) as isize;
        let mut end = match arg_int(&args, 1) {
            Some(e) => e.max(0) as isize,
            None => len,
        };
        if start > len {
            start = len;
        }
        if end > len {
            end = len;
        }
        if start > end {
            std::mem::swap(&mut start, &mut end);
        }
        return Value::string(
            chars[start as usize..end as usize]
                .iter()
                .collect::<String>(),
        );
    }
    Value::undefined()
}

fn str_index_of(this: &Value, args: Vec<Value>) -> isize {
    let needle = arg_str(&args, 0);
    let from = arg_int(&args, 1).unwrap_or(0).max(0) as usize;
    if let ValueData::String(s) = this.data() {
        // Byte-based substring search suffices for BMP; convert found offset to
        // char index.
        let hay: &str = s.as_str();
        if let Some(rel) = hay[from..].find(&needle) {
            let byte = from + rel;
            let char_idx = hay[..byte].chars().count();
            return char_idx as isize;
        }
    }
    -1
}

fn str_repeat(this: &Value, args: Vec<Value>) -> Value {
    let n = arg_int(&args, 0).unwrap_or(0).max(0) as usize;
    if let ValueData::String(s) = this.data() {
        return Value::string(s.as_str().repeat(n));
    }
    Value::undefined()
}

fn str_split(this: &Value, args: Vec<Value>) -> Value {
    let sep = arg_str(&args, 0);
    if let ValueData::String(s) = this.data() {
        if sep.is_empty() {
            // Split into characters.
            return make_array(
                s.as_str()
                    .chars()
                    .map(|c| Value::string(c.to_string()))
                    .collect(),
            );
        }
        let parts: Vec<Value> = s.as_str().split(&sep).map(Value::string).collect();
        return make_array(parts);
    }
    make_array(vec![this.clone()])
}

fn str_concat(this: &Value, args: Vec<Value>) -> Value {
    let mut out = to_string(this);
    for a in args {
        out.push_str(&to_string(&a));
    }
    Value::string(out)
}

// ---- sort / flat / fill / at (Array) --------------------------------------

fn arr_sort(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let has_cmp = !args.is_empty() && !args[0].is_undefined();
    let cmp = args.get(0).cloned();
    let len = array_len(this);
    let mut vals: Vec<Value> = (0..len).map(|i| array_get(this, i)).collect();
    if let Some(cmp) = cmp {
        if has_cmp {
            // Selection sort calling the JS comparator (stable, simple).
            for i in 0..vals.len() {
                let mut best = i;
                for j in (i + 1)..vals.len() {
                    let r = interp.call_value(
                        module,
                        cmp.clone(),
                        vec![vals[j].clone(), vals[best].clone()],
                        Value::undefined(),
                    )?;
                    let neg = match r.data() {
                        ValueData::Integer(n) => *n < 0,
                        ValueData::Number(n) => *n < 0.0,
                        _ => false,
                    };
                    if neg {
                        best = j;
                    }
                }
                vals.swap(i, best);
            }
        }
    } else {
        // Default: lexicographic by string coercion.
        vals.sort_by(|a, b| to_string(a).cmp(&to_string(b)));
    }
    // Write sorted values back into the array, in place.
    if let ValueData::Object(o) = this.data() {
        let mut b = o.borrow_mut();
        for (i, v) in vals.into_iter().enumerate() {
            b.properties.insert(
                i.to_string(),
                js_runtime::object::PropertyDescriptor::data(v),
            );
        }
    }
    Ok(NativeResult::Value(this.clone()))
}

fn arr_flat(this: &Value, args: Vec<Value>) -> Value {
    let depth = arg_int(&args, 0).unwrap_or(1).max(0) as usize;
    fn flatten(vals: &[Value], depth: usize, out: &mut Vec<Value>) {
        for v in vals {
            if depth > 0 && matches!(v.data(), ValueData::Object(o) if o.borrow().is_exotic_array) {
                let len = array_len(v);
                let sub: Vec<Value> = (0..len).map(|i| array_get(v, i)).collect();
                flatten(&sub, depth - 1, out);
            } else {
                out.push(v.clone());
            }
        }
    }
    let len = array_len(this);
    let vals: Vec<Value> = (0..len).map(|i| array_get(this, i)).collect();
    let mut out = Vec::new();
    flatten(&vals, depth, &mut out);
    make_array(out)
}

fn arr_fill(this: &Value, args: Vec<Value>) -> Value {
    let value = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    let start = arg_int(&args, 1).unwrap_or(0).max(0) as usize;
    let end = arg_int(&args, 2)
        .map(|e| (e.max(0) as usize).min(len))
        .unwrap_or(len);
    if let ValueData::Object(o) = this.data() {
        let mut b = o.borrow_mut();
        for i in start..end {
            b.properties.insert(
                i.to_string(),
                js_runtime::object::PropertyDescriptor::data(value.clone()),
            );
        }
    }
    this.clone()
}

fn arr_at(this: &Value, args: Vec<Value>) -> Value {
    let len = array_len(this) as isize;
    let i = arg_int(&args, 0).unwrap_or(0) as isize;
    let idx = if i < 0 { len + i } else { i };
    if idx >= 0 && idx < len {
        array_get(this, idx as usize)
    } else {
        Value::undefined()
    }
}

// ---- pad / starts / ends / replace / at (String) -------------------------

fn str_pad(this: &Value, args: Vec<Value>, is_start: bool) -> Value {
    if let ValueData::String(s) = this.data() {
        let target = arg_int(&args, 0).unwrap_or(0).max(0) as usize;
        let pad = args
            .get(1)
            .map(to_string)
            .unwrap_or_else(|| " ".to_string());
        let chars: Vec<char> = s.as_str().chars().collect();
        let mut chars = chars;
        if chars.len() >= target || pad.is_empty() {
            return Value::string(chars.into_iter().collect::<String>());
        }
        let need = target - chars.len();
        let pad_chars: Vec<char> = pad.chars().collect();
        let mut filler: Vec<char> = Vec::new();
        while filler.len() < need {
            filler.extend_from_slice(&pad_chars);
        }
        filler.truncate(need);
        if is_start {
            filler.extend_from_slice(&chars);
            Value::string(filler.into_iter().collect::<String>())
        } else {
            chars.extend_from_slice(&filler);
            Value::string(chars.into_iter().collect::<String>())
        }
    } else {
        Value::undefined()
    }
}

fn str_starts_ends(this: &Value, args: Vec<Value>, is_starts: bool) -> Value {
    if let ValueData::String(s) = this.data() {
        let needle = arg_str(&args, 0);
        let hay = s.as_str();
        let res = if is_starts {
            let start = arg_int(&args, 1).unwrap_or(0).max(0) as usize;
            hay[start..].starts_with(&needle)
        } else {
            let end = match arg_int(&args, 1) {
                Some(e) => (e.max(0) as usize).min(hay.len()),
                None => hay.len(),
            };
            hay[..end].ends_with(&needle)
        };
        return Value::boolean(res);
    }
    Value::boolean(false)
}

/// `String.prototype.replace(search, replacement)` — supports:
/// - string pattern (first occurrence) and RegExp pattern (global if `/g`),
/// - string replacement with `$& $1..$9 $<name> $$` substitution, and
/// - function replacement `fn(match, p1, p2, …, offset, string)`.
fn str_replace(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let s = match this.data() {
        ValueData::String(s) => s.as_str().to_string(),
        _ => return Ok(NativeResult::Value(Value::undefined())),
    };
    let search = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let repl = args.get(1).cloned().unwrap_or_else(Value::undefined);
    let is_fn = matches!(repl.data(), ValueData::Function(_));

    let is_regex = matches!(search.data(), ValueData::Object(o) if o.borrow().class == "RegExp");
    if is_regex {
        let prog = match compile_regex_obj(&search) {
            Some(p) => p,
            None => return Ok(NativeResult::Value(Value::string(s))),
        };
        let names = prog.names.clone();
        let global = regex_flag(&search, "global");
        let matches = if global {
            prog.find_all(&s, 0)
        } else {
            prog.run(&s, 0).map(|m| vec![m]).unwrap_or_default()
        };

        let mut out = String::new();
        let mut last = 0usize;
        for m in &matches {
            let (ms, me) = m.full_match().unwrap_or((last, last));
            out.push_str(&s[last..ms]);
            let replacement = if is_fn {
                let mut call_args = vec![Value::string(s[ms..me].to_string())];
                for g in 1..m.captures.len() {
                    match m.captures.get(g).copied().flatten() {
                        Some((gs, ge)) => call_args.push(Value::string(s[gs..ge].to_string())),
                        None => call_args.push(Value::undefined()),
                    }
                }
                call_args.push(Value::integer(ms as i32));
                call_args.push(Value::string(s.clone()));
                let r = interp.call_value(module, repl.clone(), call_args, Value::undefined())?;
                to_string(&r)
            } else {
                substitute_replacement(&to_string(&repl), &s, m, &names)
            };
            out.push_str(&replacement);
            last = me;
        }
        out.push_str(&s[last..]);
        return Ok(NativeResult::Value(Value::string(out)));
    }

    // ---- String pattern: replace the first occurrence. ----
    let needle = to_string(&search);
    if let Some(idx) = s.find(&needle) {
        let end = idx + needle.len();
        let replacement = if is_fn {
            let r = interp.call_value(
                module,
                repl,
                vec![
                    Value::string(needle.clone()),
                    Value::integer(idx as i32),
                    Value::string(s.clone()),
                ],
                Value::undefined(),
            )?;
            to_string(&r)
        } else {
            let m = crate::regex::RegexMatch {
                captures: vec![Some((idx, end))],
            };
            substitute_replacement(&to_string(&repl), &s, &m, &[])
        };
        let mut out = String::with_capacity(s.len() + replacement.len());
        out.push_str(&s[..idx]);
        out.push_str(&replacement);
        out.push_str(&s[end..]);
        Ok(NativeResult::Value(Value::string(out)))
    } else {
        Ok(NativeResult::Value(Value::string(s)))
    }
}

/// Read a boolean flag property off a RegExp object.
fn regex_flag(obj: &Value, name: &str) -> bool {
    let v = crate::interp::get_property(obj, &Value::string(name));
    matches!(v.data(), ValueData::Boolean(true))
}

/// Expand `$`-escapes in a replacement string against a single match.
/// Supports: `$$`, `$&` (whole match), `` $` `` (before), `$'` (after),
/// `$1`..`$9` (capture groups, two-digit aware), `$<name>` (named group).
fn substitute_replacement(
    repl: &str,
    input: &str,
    m: &crate::regex::RegexMatch,
    names: &[(String, usize)],
) -> String {
    let chars: Vec<char> = repl.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        if i + 1 >= chars.len() {
            out.push('$');
            i += 1;
            continue;
        }
        let n = chars[i + 1];
        match n {
            '$' => {
                out.push('$');
                i += 2;
            }
            '&' => {
                if let Some((s, e)) = m.full_match() {
                    out.push_str(&input[s..e]);
                }
                i += 2;
            }
            '`' => {
                if let Some((s, _)) = m.full_match() {
                    out.push_str(&input[..s]);
                }
                i += 2;
            }
            '\'' => {
                if let Some((_, e)) = m.full_match() {
                    out.push_str(&input[e..]);
                }
                i += 2;
            }
            '<' => {
                // named group $<name>
                let mut j = i + 2;
                let mut name = String::new();
                while j < chars.len() && chars[j] != '>' {
                    name.push(chars[j]);
                    j += 1;
                }
                if j < chars.len() {
                    if let Some((_, g)) = names.iter().find(|(nm, _)| nm == &name) {
                        if let Some((s, e)) = m.captures.get(*g).copied().flatten() {
                            out.push_str(&input[s..e]);
                        }
                    }
                    i = j + 1;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            d if d.is_ascii_digit() => {
                // Greedily collect digits, prefer the longest existing group number.
                let mut j = i + 1;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let digits = &chars[i + 1..j];
                let parsed: String = digits.iter().collect();
                let two: Option<usize> = if parsed.len() >= 2 {
                    parsed[..2]
                        .parse::<usize>()
                        .ok()
                        .filter(|&g| g < m.captures.len() && m.captures[g].is_some())
                } else {
                    None
                };
                if let Some(g) = two {
                    if let Some((s, e)) = m.captures.get(g).copied().flatten() {
                        out.push_str(&input[s..e]);
                    }
                    i += 3;
                } else if let Ok(one) = parsed[..1].parse::<usize>() {
                    if one < m.captures.len() {
                        if let Some((s, e)) = m.captures.get(one).copied().flatten() {
                            out.push_str(&input[s..e]);
                        }
                    }
                    i += 2;
                } else {
                    out.push('$');
                    i += 1;
                }
            }
            _ => {
                out.push('$');
                out.push(n);
                i += 2;
            }
        }
    }
    out
}

fn str_at(this: &Value, args: Vec<Value>) -> Value {
    if let ValueData::String(s) = this.data() {
        let chars: Vec<char> = s.as_str().chars().collect();
        let len = chars.len() as isize;
        let i = arg_int(&args, 0).unwrap_or(0) as isize;
        let idx = if i < 0 { len + i } else { i };
        if idx >= 0 && idx < len {
            return Value::string(chars[idx as usize].to_string());
        }
    }
    Value::undefined()
}

// ---- Callback Array methods ----------------------------------------------
//
// These invoke a user-supplied JS callback via `Interpreter::call_value`, which
// runs a sub-dispatch loop until the callback returns. The callback is called
// with `(element, index, array)` (reduce gets `(acc, element, index, array)`).

/// Call `cb(element, index, array)` and return its result.
fn call_cb(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    cb: &Value,
    arr: &Value,
    element: Value,
    index: i32,
) -> Result<Value, InterpError> {
    interp.call_value(
        module,
        cb.clone(),
        vec![element, Value::integer(index), arr.clone()],
        Value::undefined(),
    )
}

fn arr_map(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let cb = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let elt = array_get(this, i);
        out.push(call_cb(interp, module, &cb, this, elt, i as i32)?);
    }
    Ok(NativeResult::Value(make_array(out)))
}

fn arr_filter(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let cb = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    let mut out = Vec::new();
    for i in 0..len {
        let elt = array_get(this, i);
        let keep = call_cb(interp, module, &cb, this, elt.clone(), i as i32)?;
        if is_truthy(&keep) {
            out.push(elt);
        }
    }
    Ok(NativeResult::Value(make_array(out)))
}

fn arr_for_each(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let cb = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    for i in 0..len {
        let elt = array_get(this, i);
        call_cb(interp, module, &cb, this, elt, i as i32)?;
    }
    Ok(NativeResult::Value(Value::undefined()))
}

fn arr_reduce(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let cb = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let has_init = args.len() > 1;
    let mut acc = args.get(1).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    let start = if has_init { 0 } else { 1 };
    if !has_init && len > 0 {
        acc = array_get(this, 0);
    }
    for i in start..len {
        let elt = array_get(this, i);
        acc = interp.call_value(
            module,
            cb.clone(),
            vec![acc, elt, Value::integer(i as i32), this.clone()],
            Value::undefined(),
        )?;
    }
    Ok(NativeResult::Value(acc))
}

fn arr_find_cb(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
) -> Result<NativeResult, InterpError> {
    let cb = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    for i in 0..len {
        let elt = array_get(this, i);
        let found = call_cb(interp, module, &cb, this, elt.clone(), i as i32)?;
        if is_truthy(&found) {
            return Ok(NativeResult::Value(elt));
        }
    }
    Ok(NativeResult::Value(Value::undefined()))
}

/// `some` (find_any=true: stop on first truthy) and `every` (find_any=false:
/// stop on first falsy) share this shape.
fn arr_some_every(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    this: &Value,
    args: Vec<Value>,
    is_every: bool,
) -> Result<NativeResult, InterpError> {
    let cb = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let len = array_len(this);
    let default = is_every; // every([]) = true, some([]) = false
    for i in 0..len {
        let elt = array_get(this, i);
        let r = call_cb(interp, module, &cb, this, elt, i as i32)?;
        let truthy = is_truthy(&r);
        if is_every && !truthy {
            return Ok(NativeResult::Value(Value::boolean(false)));
        }
        if !is_every && truthy {
            return Ok(NativeResult::Value(Value::boolean(true)));
        }
    }
    Ok(NativeResult::Value(Value::boolean(default)))
}

fn is_truthy(v: &Value) -> bool {
    !matches!(
        v.data(),
        ValueData::Undefined | ValueData::Null | ValueData::Boolean(false)
    ) && match v.data() {
        ValueData::Integer(i) => *i != 0,
        ValueData::Number(n) => *n != 0.0 && !n.is_nan(),
        ValueData::String(s) => !s.is_empty(),
        _ => true,
    }
}

// ---- global builtins ------------------------------------------------------

/// Build a native function value for builtin `id`.
fn native_fn(name: &str, id: u16) -> Value {
    let mut f = JsFunction::new(name, 0, 0);
    f.native = Some(id);
    Value::function(f)
}

fn bind_function(this: &Value, mut args: Vec<Value>) -> Result<NativeResult, InterpError> {
    let target = this.as_function().ok_or_else(|| {
        InterpError::Throw(type_error_value(
            "Function.prototype.bind receiver is not callable",
        ))
    })?;
    let bound_this = if args.is_empty() {
        Value::undefined()
    } else {
        args.remove(0)
    };
    let mut bound = target.clone();
    let fresh = JsFunction::new(format!("bound {}", target.name), target.id, 0);
    bound.object = fresh.object;
    bound.name = format!("bound {}", target.name);
    // Binding an already-bound function cannot replace its original receiver.
    // Bound arguments, however, are concatenated in binding order.
    bound.bound_this = target
        .bound_this
        .clone()
        .or_else(|| Some(Box::new(bound_this)));
    let mut bound_args = target.bound_args.clone();
    bound_args.extend(args);
    bound.bound_args = bound_args;
    Ok(NativeResult::Value(Value::function(bound)))
}

/// Construct a namespace object (Math/console/Object/...) with given properties.
fn namespace(props: Vec<(&str, Value)>) -> Value {
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        for (k, v) in props {
            b.properties.insert(
                k.to_string(),
                js_runtime::object::PropertyDescriptor::data(v),
            );
        }
    }
    Value::object(o)
}

/// Install the global builtins (`console`, `Math`, `Object`, `JSON`, `Array`,
/// `parseInt`, `Number`, …) into the realm's global map.
pub fn install_globals(globals: &mut std::collections::HashMap<String, Value>) {
    use id::*;
    globals.insert("undefined".to_string(), Value::undefined());
    globals.insert("NaN".to_string(), Value::number(f64::NAN));
    globals.insert("Infinity".to_string(), Value::number(f64::INFINITY));
    globals.insert("eval".to_string(), native_fn("eval", EVAL));

    globals.insert(
        "console".to_string(),
        namespace(vec![
            ("log", native_fn("log", CONSOLE_LOG)),
            ("error", native_fn("error", CONSOLE_ERROR)),
            ("warn", native_fn("warn", CONSOLE_WARN)),
            ("info", native_fn("log", CONSOLE_LOG)),
        ]),
    );

    globals.insert(
        "Math".to_string(),
        namespace(vec![
            ("max", native_fn("max", MATH_MAX)),
            ("min", native_fn("min", MATH_MIN)),
            ("abs", native_fn("abs", MATH_ABS)),
            ("floor", native_fn("floor", MATH_FLOOR)),
            ("ceil", native_fn("ceil", MATH_CEIL)),
            ("round", native_fn("round", MATH_ROUND)),
            ("sqrt", native_fn("sqrt", MATH_SQRT)),
            ("pow", native_fn("pow", MATH_POW)),
            ("sign", native_fn("sign", MATH_SIGN)),
            ("PI", Value::number(std::f64::consts::PI)),
            ("E", Value::number(std::f64::consts::E)),
            ("LN2", Value::number(std::f64::consts::LN_2)),
            ("LN10", Value::number(std::f64::consts::LN_10)),
        ]),
    );

    let object_prototype = namespace(vec![
        (
            "hasOwnProperty",
            native_fn("hasOwnProperty", OBJECT_HAS_OWN),
        ),
        (
            "propertyIsEnumerable",
            native_fn("propertyIsEnumerable", OBJECT_PROP_ENUM),
        ),
    ]);
    globals.insert(
        "Object".to_string(),
        namespace(vec![
            ("keys", native_fn("keys", OBJECT_KEYS)),
            ("values", native_fn("values", OBJECT_VALUES)),
            ("entries", native_fn("entries", OBJECT_ENTRIES)),
            ("assign", native_fn("assign", OBJECT_ASSIGN)),
            ("create", native_fn("create", OBJECT_CREATE)),
            ("prototype", object_prototype),
            (
                "getPrototypeOf",
                native_fn("getPrototypeOf", OBJECT_GET_PROTO),
            ),
            (
                "setPrototypeOf",
                native_fn("setPrototypeOf", OBJECT_SET_PROTO),
            ),
            (
                "isExtensible",
                native_fn("isExtensible", OBJECT_IS_EXTENSIBLE),
            ),
            (
                "preventExtensions",
                native_fn("preventExtensions", OBJECT_PREVENT_EXTENSIONS),
            ),
            (
                "getOwnPropertyDescriptor",
                native_fn("getOwnPropertyDescriptor", OBJECT_GET_OWN_DESC),
            ),
            (
                "defineProperty",
                native_fn("defineProperty", OBJECT_DEFINE_PROP),
            ),
            (
                "getOwnPropertyNames",
                native_fn("getOwnPropertyNames", OBJECT_GET_OWN_NAMES),
            ),
            (
                "getOwnPropertySymbols",
                native_fn("getOwnPropertySymbols", OBJECT_GET_OWN_SYMBOLS),
            ),
            ("freeze", native_fn("freeze", OBJECT_FREEZE)),
            ("isFrozen", native_fn("isFrozen", OBJECT_IS_FROZEN)),
        ]),
    );
    globals.insert(
        "Reflect".to_string(),
        namespace(vec![
            (
                "defineProperty",
                native_fn("defineProperty", REFLECT_DEFINE_PROP),
            ),
            (
                "deleteProperty",
                native_fn("deleteProperty", REFLECT_DELETE_PROP),
            ),
            ("get", native_fn("get", REFLECT_GET)),
            ("has", native_fn("has", REFLECT_HAS)),
            (
                "preventExtensions",
                native_fn("preventExtensions", REFLECT_PREVENT_EXTENSIONS),
            ),
            ("set", native_fn("set", REFLECT_SET)),
            ("ownKeys", native_fn("ownKeys", REFLECT_OWN_KEYS)),
        ]),
    );

    globals.insert(
        "JSON".to_string(),
        namespace(vec![
            ("stringify", native_fn("stringify", JSON_STRINGIFY)),
            ("parse", native_fn("parse", JSON_PARSE)),
        ]),
    );

    globals.insert("Array".to_string(), native_fn("Array", ARRAY_CTOR));
    globals.insert("Map".to_string(), native_fn("Map", MAP_CTOR));
    globals.insert("Set".to_string(), native_fn("Set", SET_CTOR));
    globals.insert("Symbol".to_string(), native_fn("Symbol", SYMBOL_FN));

    globals.insert("parseInt".to_string(), native_fn("parseInt", PARSE_INT));
    globals.insert(
        "parseFloat".to_string(),
        native_fn("parseFloat", PARSE_FLOAT),
    );
    globals.insert("isNaN".to_string(), native_fn("isNaN", IS_NAN));
    globals.insert("isFinite".to_string(), native_fn("isFinite", IS_FINITE));
    globals.insert("Number".to_string(), native_fn("Number", NUMBER_FN));
    globals.insert("String".to_string(), native_fn("String", STRING_FN));
    globals.insert("Boolean".to_string(), native_fn("Boolean", BOOLEAN_FN));
    globals.insert("Promise".to_string(), native_fn("Promise", PROMISE_CTOR));
    // Error constructors.
    globals.insert("Error".to_string(), native_fn("Error", ERROR_CTOR));
    globals.insert(
        "TypeError".to_string(),
        native_fn("TypeError", TYPE_ERROR_CTOR),
    );
    globals.insert(
        "RangeError".to_string(),
        native_fn("RangeError", RANGE_ERROR_CTOR),
    );
    globals.insert(
        "SyntaxError".to_string(),
        native_fn("SyntaxError", SYNTAX_ERROR_CTOR),
    );
    globals.insert(
        "ReferenceError".to_string(),
        native_fn("ReferenceError", REF_ERROR_CTOR),
    );
}

fn math_min_max(args: Vec<Value>, is_min: bool) -> Value {
    let mut best: Option<f64> = None;
    for a in args {
        let n = arg_f64(&[a], 0).unwrap_or(f64::NAN);
        if n.is_nan() {
            return Value::number(f64::NAN);
        }
        best = Some(match best {
            None => n,
            Some(b) => {
                if is_min {
                    n.min(b)
                } else {
                    n.max(b)
                }
            }
        });
    }
    match best {
        Some(n) if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 => {
            Value::integer(n as i32)
        }
        Some(n) => Value::number(n),
        None => Value::number(if is_min {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }),
    }
}

fn math_round(n: f64) -> f64 {
    // JS Math.round: round half toward +Infinity.
    (n + 0.5).floor()
}

fn math_sign(n: f64) -> Value {
    if n.is_nan() {
        return Value::number(f64::NAN);
    }
    Value::integer(if n > 0.0 {
        1
    } else if n < 0.0 {
        -1
    } else {
        0
    })
}

fn object_keys(args: &[Value]) -> Value {
    let target = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let keys: Vec<Value> = match target.data() {
        ValueData::Object(o) => {
            let b = o.borrow();
            if let Some(namespace) = &b.module_namespace {
                namespace.keys().map(|k| Value::string(k.clone())).collect()
            } else if b.is_exotic_array {
                (0..array_len(&target))
                    .map(|i| Value::string(i.to_string()))
                    .collect()
            } else {
                b.properties
                    .keys()
                    .map(|k| Value::string(k.clone()))
                    .collect()
            }
        }
        ValueData::String(s) => (0..s.chars().count())
            .map(|i| Value::string(i.to_string()))
            .collect(),
        _ => Vec::new(),
    };
    make_array(keys)
}

fn object_values(args: &[Value]) -> Value {
    let target = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let vals: Vec<Value> = match target.data() {
        ValueData::Object(o) => {
            let b = o.borrow();
            if let Some(namespace) = &b.module_namespace {
                namespace
                    .values()
                    .filter_map(|binding| binding.get().ok())
                    .collect()
            } else if b.is_exotic_array {
                (0..array_len(&target))
                    .map(|i| array_get(&target, i))
                    .collect()
            } else {
                b.properties
                    .values()
                    .filter_map(|d| match d {
                        js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                            Some(value.clone())
                        }
                        _ => None,
                    })
                    .collect()
            }
        }
        _ => Vec::new(),
    };
    make_array(vals)
}

fn object_entries(args: &[Value]) -> Value {
    let target = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let keys = match object_keys(args).data().clone() {
        ValueData::Object(_) => object_keys(args),
        _ => return make_array(Vec::new()),
    };
    let _ = keys;
    let mut entries: Vec<Value> = Vec::new();
    if let ValueData::Object(o) = target.data() {
        let b = o.borrow();
        if let Some(namespace) = &b.module_namespace {
            for (key, binding) in namespace {
                if let Ok(value) = binding.get() {
                    entries.push(make_array(vec![Value::string(key.clone()), value]));
                }
            }
            return make_array(entries);
        }
        for (k, d) in b.properties.iter() {
            if k == "length" && b.is_exotic_array {
                continue;
            }
            if let js_runtime::object::PropertyDescriptor::Data { value, .. } = d {
                entries.push(make_array(vec![Value::string(k.clone()), value.clone()]));
            }
        }
    }
    make_array(entries)
}

fn object_assign(args: Vec<Value>) -> Value {
    let target = args.get(0).cloned().unwrap_or_else(Value::undefined);
    for src in args.iter().skip(1) {
        if let ValueData::Object(o) = src.data() {
            let b = o.borrow();
            for (k, d) in b.properties.iter() {
                if let js_runtime::object::PropertyDescriptor::Data { value, .. } = d {
                    crate::interp::set_property(&target, &Value::string(k.clone()), value.clone());
                }
            }
        }
    }
    target
}

fn parse_int(args: &[Value]) -> Value {
    let s = args.get(0).map(to_string).unwrap_or_default();
    let radix = arg_int(args, 1).filter(|&r| r != 0);
    let n = parse_int_impl(s.trim(), radix);
    match n {
        Some(n) if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 => {
            Value::integer(n as i32)
        }
        Some(n) => Value::number(n),
        None => Value::number(f64::NAN),
    }
}

fn parse_int_impl(s: &str, radix: Option<i32>) -> Option<f64> {
    let mut chars = s.chars().peekable();
    let mut sign = 1.0;
    if matches!(chars.peek(), Some('+')) {
        chars.next();
    } else if matches!(chars.peek(), Some('-')) {
        sign = -1.0;
        chars.next();
    }
    let mut base = radix.unwrap_or(10) as u32;
    if base == 16 || (radix.is_none() && matches!(chars.peek(), Some('0'))) {
        // Allow `0x` prefix only when base 16 (or unspecified leading-0 → still
        // decimal in modern JS, but accept 0x for base 16).
    }
    if (base == 16 || radix.is_none()) && chars.peek() == Some(&'0') {
        let mut clone = chars.clone();
        clone.next();
        if matches!(clone.peek(), Some('x') | Some('X')) {
            chars.next();
            chars.next();
            base = 16;
        }
    }
    if !(2..=36).contains(&base) {
        return None;
    }
    let mut acc: f64 = 0.0;
    let mut any = false;
    while let Some(&c) = chars.peek() {
        let d = c.to_digit(base);
        match d {
            Some(d) => {
                acc = acc * base as f64 + d as f64;
                any = true;
                chars.next();
            }
            None => break,
        }
    }
    if any {
        Some(sign * acc)
    } else {
        None
    }
}

fn parse_float(args: &[Value]) -> Value {
    let s = args.get(0).map(to_string).unwrap_or_default();
    let trimmed = s.trim_start();
    // Parse the longest valid float prefix.
    let end = trimmed
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E'))
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    let candidate = &trimmed[..end];
    match candidate.parse::<f64>() {
        Ok(n) => {
            if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 {
                Value::integer(n as i32)
            } else {
                Value::number(n)
            }
        }
        Err(_) => {
            if trimmed.starts_with("Infinity") {
                Value::number(f64::INFINITY)
            } else {
                Value::number(f64::NAN)
            }
        }
    }
}

fn to_number(args: &[Value]) -> Value {
    if args.is_empty() {
        return Value::integer(0);
    }
    let v = args.get(0).cloned().unwrap_or_else(Value::undefined);
    match v.data() {
        ValueData::Integer(i) => Value::integer(*i),
        ValueData::Number(n) => Value::number(*n),
        ValueData::Boolean(b) => Value::integer(if *b { 1 } else { 0 }),
        ValueData::Null => Value::integer(0),
        ValueData::Undefined => Value::number(f64::NAN),
        ValueData::String(s) => {
            let t = s.trim();
            if t.is_empty() {
                Value::integer(0)
            } else if let Ok(n) = t.parse::<f64>() {
                if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 {
                    Value::integer(n as i32)
                } else {
                    Value::number(n)
                }
            } else if t == "Infinity" || t == "+Infinity" {
                Value::number(f64::INFINITY)
            } else if t == "-Infinity" {
                Value::number(f64::NEG_INFINITY)
            } else {
                Value::number(f64::NAN)
            }
        }
        _ => Value::number(f64::NAN),
    }
}

fn json_stringify(args: &[Value]) -> Value {
    let v = args.get(0).cloned().unwrap_or_else(Value::undefined);
    match json_str(&v) {
        Some(s) => Value::string(s),
        None => Value::undefined(),
    }
}

fn json_str(v: &Value) -> Option<String> {
    match v.data() {
        ValueData::Undefined | ValueData::Function(_) | ValueData::Symbol(_) => None,
        ValueData::Null => Some("null".to_string()),
        ValueData::Boolean(b) => Some(b.to_string()),
        ValueData::Integer(i) => Some(i.to_string()),
        ValueData::Number(n) => {
            if n.is_nan() || n.is_infinite() {
                Some("null".to_string())
            } else {
                Some(json_number(*n))
            }
        }
        ValueData::String(s) => Some(json_quote(s.as_str())),
        ValueData::BigInt(_) => None,
        ValueData::Generator(_) => None,
        ValueData::Object(o) => {
            let b = o.borrow();
            if b.is_exotic_array {
                let len = b
                    .properties
                    .get("length")
                    .and_then(|d| match d {
                        js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                            match value.data() {
                                ValueData::Integer(i) => Some(*i as usize),
                                ValueData::Number(n) => Some(*n as usize),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                let mut parts = Vec::new();
                for i in 0..len {
                    let elt = b
                        .properties
                        .get(&i.to_string())
                        .and_then(|d| match d {
                            js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                                Some(value.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(Value::undefined);
                    parts.push(json_str(&elt).unwrap_or_else(|| "null".to_string()));
                }
                Some(format!("[{}]", parts.join(",")))
            } else {
                let mut parts = Vec::new();
                for (k, d) in b.properties.iter() {
                    if let js_runtime::object::PropertyDescriptor::Data { value, .. } = d {
                        if let Some(val) = json_str(value) {
                            parts.push(format!("{}:{}", json_quote(k), val));
                        }
                    }
                }
                Some(format!("{{{}}}", parts.join(",")))
            }
        }
    }
}

fn json_quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn json_number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

// ---- Error constructors ---------------------------------------------------

/// Create (or populate) an Error object with `name` and `message`.
/// Works for both `new TypeError("m")` (this=fresh obj) and `TypeError("m")`
/// (this=undefined → create a new object).
pub fn error_ctor(this: &Value, args: &[Value], name: &str) -> Value {
    let obj = if matches!(this.data(), ValueData::Object(_)) {
        this.clone()
    } else {
        Value::object(js_runtime::object::ObjectData::new_handle())
    };
    crate::interp::set_property(&obj, &Value::string("name"), Value::string(name));
    let msg = args
        .get(0)
        .map(to_string)
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    crate::interp::set_property(&obj, &Value::string("message"), Value::string(msg));
    obj
}

// ---- test262 harness natives ----------------------------------------------
//
// The runtime conformance runner (`js-test262 --execute`) drives programs that
// call the test262 assertion API. Rather than evaluate the upstream `assert.js`
// (which leans on features this engine lacks), the core API is implemented
// directly as natives: `assert.sameValue` / `assert.notSameValue` /
// `assert.throws`, the `Test262Error` constructor, and `$DONE`.

/// Build a fresh Test262Error object carrying `message`.
fn test262_error(msg: &str) -> Value {
    let obj = Value::object(js_runtime::object::ObjectData::new_handle());
    crate::interp::set_property(&obj, &Value::string("name"), Value::string("Test262Error"));
    crate::interp::set_property(&obj, &Value::string("message"), Value::string(msg));
    obj
}

/// `Object.is` semantics (NaN equals NaN; -0 differs from +0) — what test262's
/// `assert.sameValue` checks. Numbers are normalized through `f64`.
fn same_value(a: &Value, b: &Value) -> bool {
    let na = arg_f64(std::slice::from_ref(a), 0);
    let nb = arg_f64(std::slice::from_ref(b), 0);
    match (na, nb) {
        // Both numeric (covers Integer and Number): use SameValue rules.
        (Some(x), Some(y)) => {
            if x.is_nan() && y.is_nan() {
                true
            } else if x == 0.0 && y == 0.0 {
                x.is_sign_positive() == y.is_sign_positive()
            } else {
                x == y
            }
        }
        _ => eq_strict(a.clone(), b.clone()),
    }
}

/// `assert.sameValue` / `assert.notSameValue`. `negate` selects the latter.
fn assert_same_value(args: &[Value], negate: bool) -> Result<NativeResult, InterpError> {
    let actual = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let expected = args.get(1).cloned().unwrap_or_else(Value::undefined);
    let same = same_value(&actual, &expected);
    let ok = if negate { !same } else { same };
    if ok {
        return Ok(NativeResult::Value(Value::undefined()));
    }
    let op = if negate { "not " } else { "" };
    let detail = args
        .get(2)
        .filter(|v| matches!(v.data(), ValueData::String(_)));
    let msg = match detail {
        Some(v) => to_string(v),
        None => format!(
            "Expected {}SameValue({:?}, {:?}) to be true",
            op, actual, expected
        ),
    };
    Err(InterpError::Throw(test262_error(&msg)))
}

fn assert_truthy(args: &[Value]) -> Result<NativeResult, InterpError> {
    if args.first().is_some_and(is_truthy) {
        return Ok(NativeResult::Value(Value::undefined()));
    }
    let message = args
        .get(1)
        .map(to_string)
        .unwrap_or_else(|| "Expected value to be truthy".into());
    Err(InterpError::Throw(test262_error(&message)))
}

/// `assert.throws(constructor, callable[, message])` — call `callable`, expect it
/// to throw a value whose error name matches `constructor`'s. Also accepts the
/// `(ctor, callable, predicateFn)` shape: the predicate must return truthy for
/// the thrown value. Throws Test262Error if nothing is thrown or the type/predicate
/// is wrong.
fn assert_throws(
    interp: &mut Interpreter,
    module: &BytecodeGraph<'_>,
    args: &[Value],
) -> Result<NativeResult, InterpError> {
    let ctor = args.get(0).cloned().unwrap_or_else(Value::undefined);
    // The callable is the first function-valued argument after the constructor.
    let callable = args
        .iter()
        .skip(1)
        .find(|v| v.as_function().is_some())
        .cloned();
    let callable = match callable {
        Some(f) => f,
        None => {
            return Err(InterpError::Throw(test262_error(
                "assert.throws expects a function argument",
            )))
        }
    };
    // A second function (if any) is a predicate over the thrown value.
    let predicate = args
        .iter()
        .skip(1)
        .filter(|v| v.as_function().is_some())
        .skip(1)
        .next()
        .cloned();

    let expected_name = ctor.as_function().map(|f| f.name.clone());
    let thrown = match interp.call_value(module, callable, Vec::new(), Value::undefined()) {
        Ok(_) => {
            let nm = expected_name.as_deref().unwrap_or("Error");
            let msg = format!("Expected a {nm} exception to be thrown but no exception was thrown");
            return Err(InterpError::Throw(test262_error(&msg)));
        }
        Err(InterpError::Throw(v)) => v,
        Err(other) => return Err(other),
    };
    // Type check: prefer the native instanceof table; fall back to name match
    // (covers Test262Error / EvalError / URIError / user-defined throws).
    let type_ok = instanceof_check(&thrown, &ctor)
        || match (thrown_name(&thrown), &expected_name) {
            (Some(got), Some(want)) => got == *want,
            _ => false,
        };
    if !type_ok {
        let want = expected_name.as_deref().unwrap_or("Error");
        let got = thrown_name(&thrown).unwrap_or_else(|| to_string(&thrown));
        return Err(InterpError::Throw(test262_error(&format!(
            "Expected {want} but threw {got}"
        ))));
    }
    if let Some(pred) = predicate {
        let verdict = interp.call_value(module, pred, vec![thrown.clone()], Value::undefined())?;
        if !is_truthy(&verdict) {
            return Err(InterpError::Throw(test262_error(
                "assert.throws predicate returned falsy",
            )));
        }
    }
    Ok(NativeResult::Value(thrown))
}

/// Read the `.name` property of a thrown value as a `String`.
fn thrown_name(v: &Value) -> Option<String> {
    let name_val = crate::interp::get_property(v, &Value::string("name"));
    if let ValueData::String(s) = name_val.data() {
        Some(s.as_str().to_string())
    } else {
        None
    }
}

/// `$DONE([error])` — async completion callback. With no argument (or
/// `undefined`) it signals success; any other argument is a failure, surfaced
/// as a throw so the runner observes it. (Async scheduling itself isn't
/// supported, but synchronous `$DONE(value)` rejection paths still matter.)
fn done(interp: &mut Interpreter, args: &[Value]) -> Result<NativeResult, InterpError> {
    interp.mark_test262_done();
    match args.get(0) {
        Some(v) if !matches!(v.data(), ValueData::Undefined) => Err(InterpError::Throw(v.clone())),
        _ => Ok(NativeResult::Value(Value::undefined())),
    }
}

/// Install the test262 harness globals (`assert`, `Test262Error`, `$DONE`) into a
/// realm's global map. Not installed in a plain realm — the runtime conformance
/// runner opts in.
pub fn install_test262_harness(globals: &mut std::collections::HashMap<String, Value>) {
    use id::*;
    let mut assert = JsFunction::new("assert", 0, 1);
    assert.native = Some(ASSERT);
    for (name, value) in [
        ("sameValue", native_fn("sameValue", ASSERT_SAME_VALUE)),
        (
            "notSameValue",
            native_fn("notSameValue", ASSERT_NOT_SAME_VALUE),
        ),
        ("throws", native_fn("throws", ASSERT_THROWS)),
    ] {
        assert.object.borrow_mut().properties.insert(
            name.into(),
            js_runtime::object::PropertyDescriptor::data(value),
        );
    }
    globals.insert("assert".to_string(), Value::function(assert));
    globals.insert(
        "Test262Error".to_string(),
        native_fn("Test262Error", TEST262_ERROR_CTOR),
    );
    globals.insert("$DONE".to_string(), native_fn("$DONE", DONE));
}

/// `a instanceof B`: for Error native constructors, checks `a.name`.
/// For bytecode functions (user classes) we don't track prototype chains → false.
/// `a instanceof B`: for Error native constructors, checks `a.name`.
pub fn instanceof_check(a: &Value, b: &Value) -> bool {
    use id::*;
    if let (ValueData::Object(object), Some(function)) = (a.data(), b.as_function()) {
        let target = js_runtime::object::ConstructorIdentity {
            module_index: function.module_index,
            function_id: function.id,
            native_id: function.native,
        };
        if object.borrow().constructor_chain.contains(&target) {
            return true;
        }
    }
    // Error objects created without the ordinary constructor path retain the
    // historical name-based check until Error prototypes are materialized.
    let target = match b.as_function().and_then(|f| f.native) {
        Some(ERROR_CTOR) => None, // base — matches any "*Error"
        Some(TYPE_ERROR_CTOR) => Some("TypeError"),
        Some(RANGE_ERROR_CTOR) => Some("RangeError"),
        Some(SYNTAX_ERROR_CTOR) => Some("SyntaxError"),
        Some(REF_ERROR_CTOR) => Some("ReferenceError"),
        _ => return false,
    };
    if !matches!(a.data(), ValueData::Object(_)) {
        return false;
    }
    let name_val = crate::interp::get_property(a, &Value::string("name"));
    match name_val.data() {
        ValueData::String(s) => match target {
            Some(tn) => s.as_str() == tn,
            None => s.as_str().ends_with("Error"), // Error base
        },
        _ => false,
    }
}

// ---- JSON.parse ----

fn json_parse(args: &[Value]) -> Result<NativeResult, InterpError> {
    let s = args.get(0).map(to_string).unwrap_or_default();
    let mut parser = JsonParser {
        chars: s.chars().collect(),
        pos: 0,
    };
    parser.skip_ws();
    let val = parser
        .parse_value()
        .map_err(|e| InterpError::Internal(format!("JSON.parse: {}", e)))?;
    parser.skip_ws();
    if parser.pos < parser.chars.len() {
        return Err(InterpError::Internal("JSON.parse: trailing content".into()));
    }
    Ok(NativeResult::Value(val))
}

struct JsonParser {
    chars: Vec<char>,
    pos: usize,
}

impl JsonParser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }
    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }
    fn skip_ws(&mut self) {
        while matches!(
            self.peek(),
            Some(' ') | Some('\t') | Some('\n') | Some('\r')
        ) {
            self.pos += 1;
        }
    }
    fn parse_value(&mut self) -> Result<Value, String> {
        self.skip_ws();
        match self.peek() {
            Some('{') => self.parse_object(),
            Some('[') => self.parse_array(),
            Some('"') => self.parse_string().map(Value::string),
            Some('t') => self.parse_lit("true", Value::boolean(true)),
            Some('f') => self.parse_lit("false", Value::boolean(false)),
            Some('n') => self.parse_lit("null", Value::null()),
            Some(c) if c == '-' || c.is_ascii_digit() => self.parse_number(),
            _ => Err("unexpected token".into()),
        }
    }
    fn parse_lit(&mut self, lit: &str, val: Value) -> Result<Value, String> {
        for lc in lit.chars() {
            if self.bump() != Some(lc) {
                return Err(format!("expected {}", lit));
            }
        }
        Ok(val)
    }
    fn parse_number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some('-') {
            self.bump();
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.bump();
        }
        if self.peek() == Some('.') {
            self.bump();
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            self.bump();
            if matches!(self.peek(), Some('+') | Some('-')) {
                self.bump();
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.bump();
            }
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse::<f64>()
            .map(|n| {
                if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 {
                    Value::integer(n as i32)
                } else {
                    Value::number(n)
                }
            })
            .map_err(|_| "invalid number".into())
    }
    fn parse_string(&mut self) -> Result<String, String> {
        self.bump(); // opening quote
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err("unterminated string".into()),
                Some('"') => break,
                Some('\\') => match self.bump() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some('r') => out.push('\r'),
                    Some('"') => out.push('"'),
                    Some('\\') => out.push('\\'),
                    Some('/') => out.push('/'),
                    Some('b') => out.push('\u{0008}'),
                    Some('f') => out.push('\u{000C}'),
                    Some('u') => {
                        let mut code = 0u32;
                        for _ in 0..4 {
                            let c = self.bump().ok_or("bad unicode escape")?;
                            code = code * 16 + c.to_digit(16).ok_or("bad hex")?;
                        }
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                    _ => return Err("bad escape".into()),
                },
                Some(c) => out.push(c),
            }
        }
        Ok(out)
    }
    fn parse_array(&mut self) -> Result<Value, String> {
        self.bump(); // [
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(']') {
            self.bump();
            return Ok(make_array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump() {
                Some(',') => {
                    self.skip_ws();
                }
                Some(']') => break,
                _ => return Err("expected , or ]".into()),
            }
        }
        Ok(make_array(items))
    }
    fn parse_object(&mut self) -> Result<Value, String> {
        self.bump(); // {
        self.skip_ws();
        let obj = Value::object(js_runtime::object::ObjectData::new_handle());
        if self.peek() == Some('}') {
            self.bump();
            return Ok(obj);
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.bump() != Some(':') {
                return Err("expected :".into());
            }
            let val = self.parse_value()?;
            crate::interp::set_property(&obj, &Value::string(key), val);
            self.skip_ws();
            match self.bump() {
                Some(',') => {}
                Some('}') => break,
                _ => return Err("expected , or }".into()),
            }
        }
        Ok(obj)
    }
}

// ---- Number prototype + statics ----

fn num_to_fixed(this: &Value, args: &[Value]) -> Result<NativeResult, InterpError> {
    let n = arg_f64(&[this.clone()], 0).unwrap_or(f64::NAN);
    let digits = arg_int(args, 0).unwrap_or(0).max(0) as usize;
    Ok(NativeResult::Value(Value::string(format!(
        "{:.*}",
        digits, n
    ))))
}

fn num_to_string(this: &Value, args: &[Value]) -> Result<NativeResult, InterpError> {
    let n = arg_f64(&[this.clone()], 0).unwrap_or(f64::NAN);
    let radix = arg_int(args, 0).unwrap_or(10);
    if radix == 10 {
        return Ok(NativeResult::Value(Value::string(format_number_simple(n))));
    }
    if !(2..=36).contains(&radix) {
        return Err(InterpError::Internal(
            "toString() radix must be 2..36".into(),
        ));
    }
    let int_part = n.trunc() as i64;
    let frac = n - n.trunc();
    let int_str = if int_part == 0 {
        "0".to_string()
    } else {
        let mut s = String::new();
        let mut v = int_part.abs();
        while v > 0 {
            s.push(char::from_digit((v % radix as i64) as u32, radix as u32).unwrap());
            v /= radix as i64;
        }
        if int_part < 0 {
            s.push('-');
        }
        s.chars().rev().collect()
    };
    let frac_str = if frac > 0.0 {
        let mut s = String::from(".");
        let mut f = frac;
        for _ in 0..15 {
            f *= radix as f64;
            let d = f.trunc() as u32;
            s.push(char::from_digit(d, radix as u32).unwrap_or('0'));
            f -= d as f64;
            if f < 1e-10 {
                break;
            }
        }
        s
    } else {
        String::new()
    };
    Ok(NativeResult::Value(Value::string(format!(
        "{}{}",
        int_str, frac_str
    ))))
}

fn format_number_simple(n: f64) -> String {
    if n.is_nan() {
        "NaN".into()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

fn num_is_integer(args: &[Value]) -> bool {
    match args.get(0).map(|v| v.data().clone()) {
        Some(ValueData::Integer(_)) => true,
        Some(ValueData::Number(n)) if n.fract() == 0.0 && n.is_finite() => true,
        _ => false,
    }
}

// ---- String extras ----

fn str_from_char_code(args: &[Value]) -> Result<NativeResult, InterpError> {
    let mut out = String::new();
    for a in args {
        if let Some(code) = arg_f64(&[a.clone()], 0) {
            if let Some(c) = char::from_u32(code as u32) {
                out.push(c);
            }
        }
    }
    Ok(NativeResult::Value(Value::string(out)))
}

fn str_code_point_at(this: &Value, args: &[Value]) -> Result<NativeResult, InterpError> {
    let i = arg_int(args, 0).unwrap_or(0);
    if let ValueData::String(s) = this.data() {
        if let Some(c) = s.as_str().chars().nth(i.max(0) as usize) {
            return Ok(NativeResult::Value(Value::integer(c as i32)));
        }
    }
    Ok(NativeResult::Value(Value::undefined()))
}

// ---- Regex matching (.test / .exec / String.match / String.search) --------

/// Extract source+flags from a RegExp object and compile.
fn compile_regex_obj(obj: &Value) -> Option<crate::regex::RegexProgram> {
    let src = match crate::interp::get_property(obj, &Value::string("source"))
        .data()
        .clone()
    {
        ValueData::String(s) => s.as_str().to_string(),
        _ => return None,
    };
    let flags = match crate::interp::get_property(obj, &Value::string("flags"))
        .data()
        .clone()
    {
        ValueData::String(s) => s.as_str().to_string(),
        _ => String::new(),
    };
    crate::regex::compile(&src, &flags).ok()
}

fn regex_test(this: &Value, args: &[Value]) -> Value {
    let prog = match compile_regex_obj(this) {
        Some(p) => p,
        None => return Value::boolean(false),
    };
    let input = args.get(0).map(to_string).unwrap_or_default();
    Value::boolean(prog.run(&input, 0).is_some())
}

fn regex_exec(this: &Value, args: &[Value]) -> Result<NativeResult, InterpError> {
    let prog = match compile_regex_obj(this) {
        Some(p) => p,
        None => return Ok(NativeResult::Value(Value::null())),
    };
    let input = args.get(0).map(to_string).unwrap_or_default();
    match prog.run(&input, 0) {
        Some(m) => {
            // Build result array: [fullMatch, group1, group2, ...]
            let mut items = Vec::new();
            for cap in &m.captures {
                if let Some((s, e)) = cap {
                    items.push(Value::string(input[*s..*e].to_string()));
                } else {
                    items.push(Value::undefined());
                }
            }
            // Set .index and .input on the result array.
            let arr = make_array(items);
            if let Some((start, _)) = m.full_match() {
                crate::interp::set_property(
                    &arr,
                    &Value::string("index"),
                    Value::integer(start as i32),
                );
            }
            crate::interp::set_property(&arr, &Value::string("input"), Value::string(input));
            Ok(NativeResult::Value(arr))
        }
        None => Ok(NativeResult::Value(Value::null())),
    }
}

fn str_match(this: &Value, args: &[Value]) -> Result<NativeResult, InterpError> {
    let re = args.get(0).cloned().unwrap_or_else(Value::undefined);
    // If arg is a RegExp, use it; else treat as a pattern string.
    let re_obj = if matches!(re.data(), ValueData::Object(o) if o.borrow().class == "RegExp") {
        re
    } else {
        // Wrap the string pattern in a temporary RegExp object.
        let pat = to_string(&re);
        let o = js_runtime::object::ObjectData::new_handle();
        {
            let mut b = o.borrow_mut();
            b.class = "RegExp";
            let pd = |v: Value| js_runtime::object::PropertyDescriptor::data(v);
            b.properties.insert("source".into(), pd(Value::string(pat)));
            b.properties.insert("flags".into(), pd(Value::string("")));
        }
        Value::object(o)
    };
    regex_exec(&re_obj, &[this.clone()])
}

fn str_search(this: &Value, args: &[Value]) -> isize {
    let re = args.get(0).cloned().unwrap_or_else(Value::undefined);
    let re_obj = if matches!(re.data(), ValueData::Object(o) if o.borrow().class == "RegExp") {
        re
    } else {
        let pat = to_string(&re);
        let o = js_runtime::object::ObjectData::new_handle();
        {
            let mut b = o.borrow_mut();
            b.class = "RegExp";
            let pd = |v: Value| js_runtime::object::PropertyDescriptor::data(v);
            b.properties.insert("source".into(), pd(Value::string(pat)));
            b.properties.insert("flags".into(), pd(Value::string("")));
        }
        Value::object(o)
    };
    match compile_regex_obj(&re_obj) {
        Some(prog) => {
            let input = to_string(this);
            match prog.run(&input, 0) {
                Some(m) => m.full_match().map(|(s, _)| s as isize).unwrap_or(-1),
                None => -1,
            }
        }
        None => -1,
    }
}
