//! Workspace-level smoke test (lives in `js-engine` so it has a package):
//! exercises the full pipeline through the top-level engine API and links the
//! individual front-end crates.

use js_engine::{Engine, EngineConfig, ExecutionMode};

#[test]
fn empty_script_runs_through_engine() {
    let mut engine = Engine::new(EngineConfig {
        mode: ExecutionMode::Interpret,
        ..EngineConfig::default()
    });
    let result = engine.run("").expect("empty script runs");
    assert!(result.value.is_undefined());
}

#[test]
fn lexer_and_parser_link() {
    let _toks: Vec<_> = js_lexer::tokenize("var x = 1").collect();
    let _ = js_parser::parse("");
}

#[test]
fn bytecode_and_vm_link() {
    let prog = js_parser::parse("").expect("parse");
    let module = js_bytecode::compile_program(&prog).expect("compile");
    let v = js_vm::Interpreter::fresh()
        .run_module(&module)
        .expect("run");
    assert!(v.is_undefined());
}
