//! Milestone-1 end-to-end: parse → compile → interpret the three target
//! programs through the top-level [`Engine`].

use js_engine::{Engine, EngineConfig, ExecutionMode};
use js_runtime::value::ValueData;

fn run(src: &str) -> ValueData {
    let mut engine = Engine::new(EngineConfig {
        mode: ExecutionMode::Interpret,
        ..EngineConfig::default()
    });
    let result = engine.run(src).expect("engine.run should succeed");
    result.value.data().clone()
}

#[test]
fn arithmetic_precedence() {
    // 1 + 2 * 3 == 7
    match run("1 + 2 * 3") {
        ValueData::Integer(7) => {}
        v => panic!("expected Integer(7), got {:?}", v),
    }
}

#[test]
fn var_decl_and_read() {
    // var x = 5; x  ==>  5
    match run("var x = 5; x") {
        ValueData::Integer(5) => {}
        v => panic!("expected Integer(5), got {:?}", v),
    }
}

#[test]
fn function_call() {
    // function f(){return 1} f()  ==>  1
    match run("function f(){return 1} f()") {
        ValueData::Integer(1) => {}
        v => panic!("expected Integer(1), got {:?}", v),
    }
}

#[test]
fn division_is_float() {
    // 7 / 2 == 3.5
    match run("7 / 2") {
        ValueData::Number(n) => assert!((n - 3.5).abs() < 1e-12),
        v => panic!("expected Number(3.5), got {:?}", v),
    }
}

#[test]
fn parentheses_override_precedence() {
    // (1 + 2) * 3 == 9
    match run("(1 + 2) * 3") {
        ValueData::Integer(9) => {}
        v => panic!("expected Integer(9), got {:?}", v),
    }
}

#[test]
fn if_and_while() {
    // var i = 0; var s = 0; while (i < 3) { s = s + i; i = i + 1; } s
    // sums 0+1+2 == 3
    match run("var i = 0; var s = 0; while (i < 3) { s = s + i; i = i + 1; } s") {
        ValueData::Integer(3) => {}
        v => panic!("expected Integer(3), got {:?}", v),
    }
}
