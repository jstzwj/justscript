use js_diagnostics::DiagnosticPhase;
use js_engine::{Engine, EngineError, ExecutionOutcome};
use js_syntax::ProgramKind;

#[test]
fn compile_report_retains_source_and_phase() {
    let engine = Engine::default_interpreter();
    let report = engine
        .compile_named("duplicate.js", "let value; let value;", ProgramKind::Script)
        .err()
        .expect("duplicate lexical declarations are early errors");

    assert_eq!(report.source.name, "duplicate.js");
    assert_eq!(report.first().unwrap().phase, DiagnosticPhase::EarlyError);
    assert_eq!(report.first().unwrap().code.as_deref(), Some("JS-EARLY"));
    assert!(report.to_string().contains("duplicate.js:1:"));
}

#[test]
fn bytecode_has_an_instruction_source_map() {
    let engine = Engine::default_interpreter();
    let compiled = engine
        .compile_named(
            "mapped.js",
            "function add(a, b) { return a + b; } add(1, 2);",
            ProgramKind::Script,
        )
        .expect("source compiles");

    for function in std::iter::once(&compiled.bytecode.main).chain(&compiled.bytecode.functions) {
        assert_eq!(function.code.len(), function.source_map.len());
        assert!(function.source_map.iter().all(|span| !span.is_dummy()));
    }
}

#[test]
fn yield_star_has_a_mapped_bytecode_instruction() {
    let engine = Engine::default_interpreter();
    let compiled = engine
        .compile_named(
            "delegate.js",
            "function* values() { yield* []; }",
            ProgramKind::Script,
        )
        .expect("yield* must lower successfully");
    let function = compiled
        .bytecode
        .functions
        .iter()
        .find(|function| function.name == "values")
        .expect("generator bytecode");
    assert!(function
        .code
        .iter()
        .any(|instruction| instruction.op == js_bytecode::Opcode::YieldStar));
}

#[test]
fn uncaught_exception_has_throw_site_and_javascript_stack() {
    let source = "function inner() {\n  throw new TypeError('bad');\n}\nfunction outer() { inner(); }\nouter();";
    let mut engine = Engine::default_interpreter();
    let outcome = engine.execute_named("trace.js", source, ProgramKind::Script);

    let ExecutionOutcome::Failed(EngineError::Exception(error)) = outcome else {
        panic!("expected an uncaught JavaScript exception");
    };
    assert_eq!(error.source.as_ref().unwrap().name, "trace.js");
    assert_eq!(error.value.error_name().as_deref(), Some("TypeError"));
    let throw_site = error
        .span()
        .snippet(&error.source.as_ref().unwrap().src)
        .unwrap();
    assert!(throw_site.starts_with("throw "));
    assert!(throw_site.contains("TypeError"));
    let functions: Vec<_> = error
        .stack
        .iter()
        .map(|frame| frame.function.as_str())
        .collect();
    assert_eq!(functions, ["inner", "outer", "<main>"]);

    let rendered = error.to_string();
    assert!(rendered.contains("at inner (trace.js:2:3)"));
    assert!(rendered.contains("at outer (trace.js:4:"));
}

#[test]
fn run_and_execute_share_the_same_failure_taxonomy() {
    let mut engine = Engine::default_interpreter();
    assert!(matches!(
        engine.run("throw 1;"),
        Err(EngineError::Exception(_))
    ));
    assert!(matches!(
        engine.execute("throw 1;"),
        ExecutionOutcome::Failed(EngineError::Exception(_))
    ));
}

#[test]
fn engine_fault_retains_the_current_instruction_location() {
    let mut engine = Engine::default_interpreter();
    let outcome = engine.execute_named("fault.js", "JSON.parse('{');", ProgramKind::Script);
    let ExecutionOutcome::Failed(EngineError::Fault(error)) = outcome else {
        panic!("expected a structured engine fault");
    };

    assert_eq!(error.source.as_ref().unwrap().name, "fault.js");
    assert_eq!(error.stack[0].function, "<main>");
    let site = error
        .span()
        .snippet(&error.source.as_ref().unwrap().src)
        .unwrap();
    assert!(site.starts_with("JSON.parse"));
    assert!(error.to_string().contains("fault.js:1:1"));
}

#[test]
fn successful_native_call_in_finally_preserves_original_stack() {
    let source = "function inner(){ throw 1; } function outer(){ try { inner(); } finally { String(1); } } outer();";
    let mut engine = Engine::default_interpreter();
    let outcome = engine.execute_named("finally.js", source, ProgramKind::Script);
    let ExecutionOutcome::Failed(EngineError::Exception(error)) = outcome else {
        panic!("expected the original exception to escape finally");
    };

    let functions: Vec<_> = error
        .stack
        .iter()
        .map(|frame| frame.function.as_str())
        .collect();
    assert_eq!(functions, ["inner", "outer", "<main>"]);
}
