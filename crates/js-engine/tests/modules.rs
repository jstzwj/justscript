use js_engine::{Engine, EngineError, MemoryModuleLoader, ModuleError};
use js_runtime::value::{Value, ValueData};

fn integer(value: &Value) -> i32 {
    match value.data() {
        ValueData::Integer(value) => *value,
        ValueData::Number(value) => *value as i32,
        other => panic!("expected a number, found {other:?}"),
    }
}

#[test]
fn named_import_calls_function_in_its_defining_module() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "app/dep.js",
        "export const base = 40; export function add(value) { return base + value; }",
    );
    loader.insert("app/main.js", "import { add } from './dep.js'; add(2);");

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("app/main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn default_import_and_named_reexport_link_end_to_end() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "export default 40; export const extra = 2;");
    loader.insert(
        "bridge.js",
        "export { default as base, extra } from './dep.js';",
    );
    loader.insert(
        "main.js",
        "import { base, extra } from './bridge.js'; base + extra;",
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn live_binding_and_shared_dependency_evaluate_once() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "counter.js",
        r#"
export let evaluations = 0;
evaluations += 1;
export let value = 0;
export function increment() { value += 1; }
"#,
    );
    loader.insert(
        "mutator.js",
        "import { increment } from './counter.js'; increment(); export const done = true;",
    );
    loader.insert(
        "main.js",
        r#"
import './mutator.js';
import { value, evaluations } from './counter.js';
value + evaluations;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 2);
}

#[test]
fn cyclic_graph_links_before_evaluation() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "a.js",
        "import { b } from './b.js'; export let a = 1; export const sum = () => a + b;",
    );
    loader.insert(
        "b.js",
        "import { a } from './a.js'; export let b = 2; export const seeA = () => a;",
    );
    loader.insert("main.js", "import { sum } from './a.js'; sum();");

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 3);
}

#[test]
fn missing_export_is_a_link_error() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "export const present = 1;");
    loader.insert("main.js", "import { missing } from './dep.js'; missing;");

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Module(ModuleError::Link { module, message }) = error else {
        panic!("expected a module linking error: {error:?}");
    };
    assert_eq!(module, "main.js");
    assert!(message.contains("does not export `missing`"));
}

#[test]
fn cross_module_stack_uses_each_frames_source() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "dep.js",
        "export function fail() { throw new TypeError('bad'); }",
    );
    loader.insert("main.js", "import { fail } from './dep.js'; fail();");

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Exception(exception) = error else {
        panic!("expected a JavaScript exception: {error:?}");
    };
    assert_eq!(exception.source.as_ref().unwrap().name, "dep.js");
    let rendered = exception.to_string();
    assert!(rendered.contains("at fail (dep.js:1:"), "{rendered}");
    assert!(rendered.contains("at <main> (main.js:1:"), "{rendered}");
}

#[test]
fn namespace_import_exposes_sorted_live_exports() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "dep.js",
        "export let z = 1; export let a = 2; export function update() { z = 40; }",
    );
    loader.insert(
        "main.js",
        r#"
import * as ns from './dep.js';
ns.update();
Object.keys(ns).join(',') + ':' + (ns.z + ns.a);
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::String(value) => assert_eq!(value.as_str(), "a,update,z:42"),
        other => panic!("expected a string, found {other:?}"),
    }
}

#[test]
fn namespace_reexport_produces_a_namespace_object() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "export const answer = 42;");
    loader.insert("bridge.js", "export * as dep from './dep.js';");
    loader.insert("main.js", "import { dep } from './bridge.js'; dep.answer;");

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn assigning_to_an_import_binding_throws_type_error() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "export let value = 1;");
    loader.insert("main.js", "import { value } from './dep.js'; value = 2;");

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Exception(exception) = error else {
        panic!("expected an exception: {error:?}");
    };
    assert_eq!(exception.value.error_name().as_deref(), Some("TypeError"));
}

#[test]
fn assigning_to_a_namespace_property_throws_type_error() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "export let value = 1;");
    loader.insert("main.js", "import * as ns from './dep.js'; ns.value = 2;");

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Exception(exception) = error else {
        panic!("expected an exception: {error:?}");
    };
    assert_eq!(exception.value.error_name().as_deref(), Some("TypeError"));
}

#[test]
fn cyclic_read_before_lexical_initialization_throws_reference_error() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("a.js", "import { b } from './b.js'; export const a = b;");
    loader.insert("b.js", "import { a } from './a.js'; export const b = a;");
    loader.insert("main.js", "import { a } from './a.js'; a;");

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Exception(exception) = error else {
        panic!("expected an exception: {error:?}");
    };
    assert_eq!(
        exception.value.error_name().as_deref(),
        Some("ReferenceError")
    );
}

#[test]
fn top_level_await_runs_promise_reactions_at_a_job_checkpoint() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
let ticks = [];
let result = Promise.resolve(40).then(value => {
    ticks.push(1);
    return value + 2;
});
ticks.push(0);
let answer = await result;
ticks.join(',') + ':' + answer;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::String(value) => assert_eq!(value.as_str(), "0,1:42"),
        other => panic!("expected a string, found {other:?}"),
    }
}

#[test]
fn async_function_returns_a_promise_consumed_by_top_level_await() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
async function answer() {
    return await Promise.resolve(42);
}
await answer();
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn promise_constructor_resolving_function_feeds_top_level_await() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("main.js", "await new Promise(resolve => resolve(42));");

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn top_level_await_assimilates_plain_thenables() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        "await { then: function(resolve) { resolve(42); } };",
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn top_level_await_rejects_when_a_thenable_throws() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
let caught = false;
try {
    await { then: function() { throw new RangeError('bad'); } };
} catch (error) {
    caught = error instanceof RangeError;
}
caught;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

#[test]
fn promise_reaction_adopts_the_returned_promise() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
let caught = false;
try {
    await Promise.resolve().then(function() {
        return Promise.reject(new RangeError('first'));
    });
} catch (error) {
    caught = error instanceof RangeError;
}
caught;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

#[test]
fn await_expression_in_call_argument_uses_the_resolved_value() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
let thenable = { then: function(resolve) { resolve(42); } };
assert.sameValue(await thenable, 42);
"#,
    );

    let mut engine = Engine::default_interpreter();
    engine.install_test262_harness();
    engine.run_module("main.js", &loader).unwrap();
}

#[test]
fn suspended_async_dependency_does_not_block_a_sibling_module() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("async.js", "check = false; await 0; check = true;");
    loader.insert("sync.js", "export const observed = check;");
    loader.insert(
        "main.js",
        "import './async.js'; import { observed } from './sync.js'; observed;",
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(false)));
}

#[test]
fn async_parent_modules_resume_in_depth_first_evaluation_order() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("async.js", "await 0; order = 'async';");
    loader.insert("direct-1.js", "import './async.js'; order += ':direct-1';");
    loader.insert("direct-2.js", "import './async.js'; order += ':direct-2';");
    loader.insert(
        "indirect.js",
        "import './direct-1.js'; order += ':indirect';",
    );
    loader.insert(
        "main.js",
        r#"
import './direct-1.js';
import './direct-2.js';
import './indirect.js';
order;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::String(value) => {
            assert_eq!(value.as_str(), "async:direct-1:direct-2:indirect")
        }
        other => panic!("expected a string, found {other:?}"),
    }
}

#[test]
fn await_can_supply_builtin_constructors_to_new() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
(new (await Number)).valueOf() + ':' +
(new (await String)).valueOf() + ':' +
(new (await Boolean)).valueOf() + ':' +
(new (await Array)).length + ':' +
(new (await Map)).size + ':' +
(new (await Set)).size;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::String(value) => assert_eq!(value.as_str(), "0::false:0:0:0"),
        other => panic!("expected a string, found {other:?}"),
    }
}

#[test]
fn import_defer_does_not_evaluate_until_namespace_is_observed() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "dep.js",
        "evaluations = evaluations + 1; export const value = 42;",
    );
    loader.insert(
        "main.js",
        r#"
evaluations = 0;
import defer * as ns from './dep.js';
let before = evaluations;
let answer = ns.value;
before * 100 + answer + evaluations - 1;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn unused_import_defer_never_evaluates_dependency() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "evaluations = 1; export const value = 42;");
    loader.insert(
        "main.js",
        "evaluations = 0; import defer * as ns from './dep.js'; evaluations;",
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 0);
}

#[test]
fn deferred_evaluation_runs_ordinary_dependencies_once() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "leaf.js",
        "evaluations = evaluations + 1; export const base = 40;",
    );
    loader.insert(
        "dep.js",
        "import { base } from './leaf.js'; export const answer = base + 2;",
    );
    loader.insert(
        "main.js",
        r#"
evaluations = 0;
import defer * as ns from './dep.js';
ns.answer + ns.answer + evaluations;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 85);
}

#[test]
fn cyclic_dependency_observes_hoisted_module_function() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "a.js",
        "import { seen } from './b.js'; export function answer() { return 42; } export { seen };",
    );
    loader.insert(
        "b.js",
        "import { answer } from './a.js'; export const seen = answer();",
    );
    loader.insert("main.js", "import { seen } from './a.js'; seen;");

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn module_const_binding_rejects_reassignment_after_initialization() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("main.js", "const answer = 42; answer = 0;");

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Exception(exception) = error else {
        panic!("expected an exception: {error:?}");
    };
    assert_eq!(exception.value.error_name().as_deref(), Some("TypeError"));
}

#[test]
fn unresolved_reference_throws_but_typeof_remains_undefined() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
assert.sameValue(typeof missing, 'undefined');
assert.throws(ReferenceError, function() { missing; });
"#,
    );

    let mut engine = Engine::default_interpreter();
    engine.install_test262_harness();
    engine.run_module("main.js", &loader).unwrap();
}

#[test]
fn default_export_named_evaluation_sets_function_and_class_names() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("fn.js", "export default (function() {});");
    loader.insert("class.js", "export default class {};");
    loader.insert(
        "main.js",
        r#"
import fn from './fn.js';
import Class from './class.js';
fn.name + ':' + Class.name;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::String(value) => assert_eq!(value.as_str(), "default:default"),
        other => panic!("expected a string, found {other:?}"),
    }
}

#[test]
fn anonymous_default_class_self_import_is_in_tdz_until_evaluation() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
assert.throws(ReferenceError, function() { typeof Class; });
export default class {};
import Class from './main.js';
"#,
    );

    let mut engine = Engine::default_interpreter();
    engine.install_test262_harness();
    engine.run_module("main.js", &loader).unwrap();
}

#[test]
fn hoisted_module_function_captures_linked_import_binding() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("dep.js", "export const answer = 42;");
    loader.insert(
        "main.js",
        r#"
import { answer } from './dep.js';
export function read() { return answer; }
read();
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert_eq!(integer(&result.value), 42);
}

#[test]
fn module_function_identity_is_stable_across_instantiation_and_evaluation() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "a.js",
        r#"
import { before } from './b.js';
export function fn() {}
export const stable = before === fn;
"#,
    );
    loader.insert(
        "b.js",
        "import { fn } from './a.js'; export const before = fn;",
    );
    loader.insert("main.js", "import { stable } from './a.js'; stable;");

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

// ---- typed module host: JSON / text / source-phase (PLAN C1–C4) ----

/// JSON module default export round-trips a primitive value (C3).
#[test]
fn json_module_default_export_round_trips_a_primitive() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("data.json", "  -1.2345  ");
    loader.insert(
        "main.js",
        "import value from './data.json' with { type: 'json' }; value;",
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::Number(value) => assert_eq_float(*value, -1.2345),
        ValueData::Integer(value) => assert_eq!(*value, -1),
        other => panic!("expected a number, found {other:?}"),
    }
}

/// `assert.sameValue` uses `Object.is`; the harness rounds -0/+0 but not
/// other floats. Keep a tiny epsilon helper for the JSON number round-trip.
fn assert_eq_float(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}

/// The SAME JSON module imported twice must return the SAME default object
/// identity (C3 idempotency — the graph cache keys on canonical URL + type).
#[test]
fn same_json_module_imported_twice_shares_object_identity() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("data.json", r#"{"answer": 42}"#);
    loader.insert(
        "main.js",
        r#"
import a from './data.json' with { type: 'json' };
import b from './data.json' with { type: 'json' };
a === b;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

/// A `.js`-named module imported as text yields its source as the default
/// export and is NEVER parsed as JavaScript (C3 text type selection).
#[test]
fn javascript_file_imported_as_text_is_not_parsed() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("payload.js", "this is not (valid) javascript!");
    loader.insert(
        "main.js",
        r#"
import text from './payload.js' with { type: 'text' };
text;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    match result.value.data() {
        ValueData::String(value) => {
            assert_eq!(value.as_str(), "this is not (valid) javascript!")
        }
        other => panic!("expected the unparsed text, found {other:?}"),
    }
}

/// Empty text yields the empty string (C3).
#[test]
fn empty_text_module_yields_the_empty_string() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("empty.txt", "");
    loader.insert(
        "main.js",
        "import text from './empty.txt' with { type: 'text' }; text === '';",
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

/// A JavaScript entry self-imported as text produces a DISTINCT typed record
/// (C1/C3): the same canonical URL loaded once as JavaScript and once as text
/// occupies two graph slots, so the text import does not create a false JS
/// cycle and does not parse the entry as JavaScript.
#[test]
fn javascript_entry_self_imported_as_text_is_a_distinct_record() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert(
        "main.js",
        r#"
// Self-import as text: must return this very source string, not re-enter JS.
import selfSource from './main.js' with { type: 'text' };
typeof selfSource === 'string' && selfSource.includes('Self-import as text');
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "self text-import should not create a JS cycle: {:?}",
        result.value
    );
}

/// Import-attribute SOURCE ORDERING does not alter request identity (C1):
/// `{type:"json"}` and the same pair reordered must resolve to the SAME Module
/// Record (attributes are canonicalized, not compared by source order).
#[test]
fn import_attribute_source_ordering_does_not_alter_request_identity() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("data.json", r#"{"v": 1}"#);
    loader.insert(
        "main.js",
        // Two imports of the same JSON module with attributes in different
        // orders. The graph cache must deduplicate them → one record → shared
        // default object identity.
        r#"
import a from './data.json' with { type: 'json', another: 'x' };
import b from './data.json' with { another: 'x', type: 'json' };
a === b;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

/// The SAME URL with DIFFERENT module types yields DISTINCT records (C1): the
/// graph cache key is canonical URL + module type, so `./m` as JSON and as text
/// produce two unrelated default exports.
#[test]
fn same_url_with_different_module_types_yields_distinct_records() {
    let mut loader = MemoryModuleLoader::new();
    // The same source text loaded as JSON (parsed) vs as text (raw string).
    loader.insert("m", r#"{"k": 7}"#);
    loader.insert(
        "main.js",
        r#"
import asJson from './m' with { type: 'json' };
import asText from './m' with { type: 'text' };
typeof asJson === 'object' && typeof asText === 'string' && asJson !== asText;
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(matches!(result.value.data(), ValueData::Boolean(true)));
}

/// An unsupported import `type` attribute surfaces as a structured module link
/// error, never an internal VM fault or panic (C2).
#[test]
fn unsupported_import_type_is_a_link_error() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("m.wasm", "<bytes>");
    loader.insert(
        "main.js",
        "import value from './m.wasm' with { type: 'webassembly' }; value;",
    );

    let mut engine = Engine::default_interpreter();
    let error = engine.run_module("main.js", &loader).unwrap_err();
    let EngineError::Module(ModuleError::Link { message, .. }) = error else {
        panic!("expected a module link error for unsupported type: {error:?}");
    };
    assert!(
        message.contains("unsupported import attribute") && message.contains("webassembly"),
        "unexpected message: {message}"
    );
}

/// Source-phase import binds the local immutable import slot directly to the
/// TARGET module's `module_source_cell`. Two source imports resolving to the
/// same Module Record therefore share cell/value identity (C4), which is what
/// makes a star re-export of a source binding unambiguous.
#[test]
fn source_phase_import_shares_one_module_source_across_importers() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("target.js", "export const answer = 42;");
    loader.insert("a.js", "import source s from './target.js'; export { s };");
    loader.insert("b.js", "import source s from './target.js'; export { s };");
    loader.insert(
        "main.js",
        // Both re-exported source bindings resolve to the SAME ModuleSource
        // object (the target module's `module_source_cell`), so the star
        // re-export below is unambiguous.
        r#"
export * from './a.js';
export * from './b.js';
"#,
    );
    loader.insert(
        "entry.js",
        r#"
import { s } from './main.js';
typeof s === 'object';
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("entry.js", &loader).unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "source-phase re-export should resolve to one ModuleSource: {:?}",
        result.value
    );
}

/// A source-phase import bound through a namespace re-export resolves to the
/// ModuleSource object (C4: Module Namespace Exotic Object [[Get]]).
#[test]
fn source_phase_namespace_access_returns_the_module_source() {
    let mut loader = MemoryModuleLoader::new();
    loader.insert("target.js", "export const answer = 42;");
    loader.insert(
        "bridge.js",
        "import source x from './target.js'; export { x };",
    );
    loader.insert(
        "main.js",
        r#"
import * as ns from './bridge.js';
typeof ns.x === 'object';
"#,
    );

    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "ns.x should be the ModuleSource: {:?}",
        result.value
    );
}

/// The generic filesystem/memory loader does NOT understand the Test262
/// `<module source>` host convention — it must decline (C4). The Test262 host
/// wrapper that DOES handle the sentinel lives in js-test262, not here.
#[test]
fn generic_memory_loader_declines_the_module_source_sentinel() {
    let loader = MemoryModuleLoader::new();
    // The sentinel is not a registered module and must not be silently
    // accepted by the generic loader.
    let resolved = js_engine::ModuleLoader::resolve(&loader, Some("main.js"), "<module source>");
    assert!(
        resolved.is_err(),
        "generic loader must not handle the sentinel"
    );
}

#[test]
fn json_module_values_carry_the_realm_prototypes() {
    // Regression for json-value-array.js: a JSON module's arrays and nested
    // objects must be connected to `%ArrayPrototype%` / `%ObjectPrototype%`,
    // which must be the SAME objects `Array.prototype` / `Object.prototype`
    // resolve to.
    let mut loader = MemoryModuleLoader::new();
    loader.insert("data.json", "[1, { \"k\": 2 }]");
    loader.insert(
        "main.js",
        "import value from './data.json' with { type: 'json' };\n\
         Object.getPrototypeOf(value) === Array.prototype &&\n\
         Object.getPrototypeOf(value[1]) === Object.prototype &&\n\
         Object.getOwnPropertyNames(value).length === 3;",
    );
    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "JSON prototype identity wrong: {:?}",
        result.value
    );
}

#[test]
fn dynamic_import_with_attributes_resolves_a_typed_module() {
    // Regression for json-idempotency.js: a literal
    // `import(specifier, { with: { type: 'json' } })` must preload the request
    // with its attributes so the dynamic import resolves a JSON (not JS)
    // module. The same canonical URL + type must share the record produced by
    // a static import of the same file.
    let mut loader = MemoryModuleLoader::new();
    loader.insert("data.json", "[1, 2, 3]");
    loader.insert(
        "main.js",
        "import staticDefault from './data.json' with { type: 'json' };\n\
         const ns = await import('./data.json', { with: { type: 'json' } });\n\
         Array.isArray(ns.default) && ns.default.length === 3 && ns.default === staticDefault;",
    );
    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "dynamic import attributes wrong: {:?}",
        result.value
    );
}

#[test]
fn multiple_engines_keep_their_own_realm_prototypes() {
    // Regression for the thread-local prototype bug (fix #1): per-realm
    // prototypes must not leak across engines on the same thread. Creating
    // Engine B and running it — which under the old design overwrote a
    // process-wide thread-local — must not change which prototype Engine A's
    // arrays and JSON module values link to. We run A, then B (the clobbering
    // step), then A again; A's second run must still report
    // `getPrototypeOf(value) === Array.prototype`.
    let mut loader_a = MemoryModuleLoader::new();
    loader_a.insert("data.json", "[1, { \"k\": 2 }]");
    loader_a.insert(
        "main.js",
        "import value from './data.json' with { type: 'json' };\n\
         Object.getPrototypeOf(value) === Array.prototype &&\n\
         Object.getPrototypeOf(value[1]) === Object.prototype &&\n\
         value[0] === 1 && value[1].k === 2;",
    );
    let mut loader_b = MemoryModuleLoader::new();
    loader_b.insert("data.json", "[9, 8, 7]");
    loader_b.insert(
        "main.js",
        "import value from './data.json' with { type: 'json' };\n\
         Object.getPrototypeOf(value) === Array.prototype &&\n\
         value.length === 3;",
    );

    let mut engine_a = Engine::default_interpreter();
    let mut engine_b = Engine::default_interpreter();

    let a_first = engine_a.run_module("main.js", &loader_a).unwrap();
    assert!(
        matches!(a_first.value.data(), ValueData::Boolean(true)),
        "engine A run 1 lost prototype identity: {:?}",
        a_first.value
    );
    // Engine B runs on the same thread — under the old thread-local this would
    // leave the active prototype pointing at B's realm.
    let b = engine_b.run_module("main.js", &loader_b).unwrap();
    assert!(
        matches!(b.value.data(), ValueData::Boolean(true)),
        "engine B lost prototype identity: {:?}",
        b.value
    );
    // A again, after B. A's JSON module value must still link to A's own
    // prototypes, not B's.
    let a_again = engine_a.run_module("main.js", &loader_a).unwrap();
    assert!(
        matches!(a_again.value.data(), ValueData::Boolean(true)),
        "engine A run 2 (after B) adopted B's prototype: {:?}",
        a_again.value
    );
}

#[test]
fn same_specifier_with_different_dynamic_import_types_are_distinct() {
    // Regression for the dynamic-import request-index fix (fix #2): two dynamic
    // imports of the SAME specifier with DIFFERENT `with: { type }` must resolve
    // to distinct Module Records — a JSON array vs the same text as a string —
    // not collapse onto whichever type was preloaded. The `DynamicImport` opcode
    // carries the request index, so the full ModuleRequest (specifier + phase +
    // attributes) is the identity, per TC39 Module Requests.
    let mut loader = MemoryModuleLoader::new();
    loader.insert("data.json", "[1, 2, 3]");
    loader.insert(
        "main.js",
        "const jsonNs = await import('./data.json', { with: { type: 'json' } });\n\
         const textNs = await import('./data.json', { with: { type: 'text' } });\n\
         Array.isArray(jsonNs.default) && jsonNs.default.length === 3 &&\n\
         typeof textNs.default === 'string' && textNs.default === '[1, 2, 3]' &&\n\
         jsonNs.default !== textNs.default;",
    );
    let mut engine = Engine::default_interpreter();
    let result = engine.run_module("main.js", &loader).unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "same-specifier different-type dynamic imports collapsed to one record: {:?}",
        result.value
    );
}

#[test]
fn intrinsic_prototype_chain_links_to_object_prototype() {
    // Regression for the prototype-chain completeness fix:
    //  - `Array.prototype.[[Prototype]] === Object.prototype`
    //    (test262 built-ins/Array/prototype/proto.js).
    //  - `Object()` and `new Object()` produce ordinary objects whose
    //    [[Prototype]] is the realm's `%ObjectPrototype%`.
    //  - array/object literals resolve through the same prototypes.
    let mut engine = Engine::default_interpreter();
    let result = engine
        .run(
            "Object.getPrototypeOf(Array.prototype) === Object.prototype &&\n\
         Object.getPrototypeOf(Object()) === Object.prototype &&\n\
         Object.getPrototypeOf(new Object()) === Object.prototype &&\n\
         Object.getPrototypeOf([]) === Array.prototype &&\n\
         Object.getPrototypeOf({}) === Object.prototype;",
        )
        .unwrap();
    assert!(
        matches!(result.value.data(), ValueData::Boolean(true)),
        "prototype chain incomplete: {:?}",
        result.value
    );
}

#[test]
fn same_engine_keeps_prototype_identity_and_builtin_modifications_across_runs() {
    // Regression for the once-per-realm bootstrap fix: a realm is long-lived
    // and shared across every interpreter this engine creates, so two `run`s in
    // the same Engine must share one `%ObjectPrototype%` / `%ArrayPrototype%`
    // (objects captured in run 1 still match the prototypes resolved in run 2),
    // and user modifications to built-ins must NOT be overwritten by
    // re-installation on run 2.
    let mut engine = Engine::default_interpreter();
    let first = engine
        .run(
            "globalThis.__arr = [];\n\
             globalThis.__obj = {};\n\
             globalThis.__arrayProto = Array.prototype;\n\
             globalThis.__objectProto = Object.prototype;\n\
             globalThis.__arrProto = Object.getPrototypeOf(globalThis.__arr);\n\
             JSON.__mutated = 'yes';\n\
             Object.getPrototypeOf(globalThis.__arr) === Array.prototype;",
        )
        .unwrap();
    assert!(
        matches!(first.value.data(), ValueData::Boolean(true)),
        "run 1 lost prototype identity: {:?}",
        first.value
    );
    let second = engine
        .run(
            "Object.getPrototypeOf(globalThis.__arr) === Array.prototype &&\n\
             Object.getPrototypeOf(globalThis.__obj) === Object.prototype &&\n\
             globalThis.__arrayProto === Array.prototype &&\n\
             globalThis.__objectProto === Object.prototype &&\n\
             globalThis.__arrProto === Array.prototype &&\n\
             JSON.__mutated === 'yes';",
        )
        .unwrap();
    assert!(
        matches!(second.value.data(), ValueData::Boolean(true)),
        "run 2 lost prototype identity or a builtin modification: {:?}",
        second.value
    );
}
