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
