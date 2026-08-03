//! Regression tests for update expressions (`++` / `--`) whose target is a
//! parenthesized identifier.
//!
//! Background: `compile_update` rejected `Expr::Paren` targets with
//! `compile error: invalid update target`, which failed the 8 Test262
//! `target-cover-id.js` variants. ECMA-262 treats parentheses as transparent
//! for static semantics, so `(x)++`, `((x))--`, `++(x)`, and `--((x))` must
//! behave exactly like their unparenthesized forms. The compiler now recurses
//! through `Expr::Paren` before dispatching on the underlying target.

use js_bytecode::{compile_program, verify_module};
use js_runtime::{Value, ValueData};
use js_vm::Interpreter;

fn run(src: &str) -> Value {
    let prog = js_parser::parse(src).expect("parse");
    let module = compile_program(&prog).expect("compile");
    verify_module(&module).expect("bytecode must verify");
    Interpreter::fresh()
        .run_module(&module)
        .unwrap_or_else(|e| panic!("run failed: {:?}", e))
}

fn as_i64(v: &Value) -> i64 {
    match v.data() {
        ValueData::Integer(i) => *i as i64,
        ValueData::Number(n) => *n as i64,
        other => panic!("expected number, got {:?}", other),
    }
}

/// `compile_update` must accept a parenthesized target — this is the direct
/// regression guard for the "invalid update target" compile error.
#[test]
fn parenthesized_update_target_compiles() {
    // Each of these must compile and verify without a diagnostic.
    for src in [
        "var x = 1; (x)++",
        "var x = 1; (x)--",
        "var x = 1; ++(x)",
        "var x = 1; --(x)",
        "var x = 1; ((x))++",
        "var x = 1; ((x))--",
        "var x = 1; ++((x))",
        "var x = 1; --((x))",
    ] {
        let prog = js_parser::parse(src).expect("parse");
        let module = compile_program(&prog)
            .unwrap_or_else(|_| panic!("parenthesized update must compile: {}", src));
        verify_module(&module).expect("verify");
    }
}

#[test]
fn postfix_increment_on_parenthesized_identifier() {
    // `(x)++` must mutate x and the completion value is the pre-increment value.
    assert_eq!(as_i64(&run("var x = 5; (x)++; x")), 6);
    let pre = run("var x = 5; (x)++");
    assert_eq!(
        as_i64(&pre),
        5,
        "postfix completion is the pre-increment value"
    );
}

#[test]
fn postfix_decrement_on_parenthesized_identifier() {
    assert_eq!(as_i64(&run("var x = 5; (x)--; x")), 4);
    let pre = run("var x = 5; (x)--");
    assert_eq!(
        as_i64(&pre),
        5,
        "postfix completion is the pre-decrement value"
    );
}

#[test]
fn prefix_increment_on_parenthesized_identifier() {
    // `++(x)` must mutate x and the completion value is the post-increment value.
    assert_eq!(as_i64(&run("var x = 5; ++(x); x")), 6);
    let post = run("var x = 5; ++(x)");
    assert_eq!(
        as_i64(&post),
        6,
        "prefix completion is the post-increment value"
    );
}

#[test]
fn prefix_decrement_on_parenthesized_identifier() {
    assert_eq!(as_i64(&run("var x = 5; --(x); x")), 4);
    let post = run("var x = 5; --(x)");
    assert_eq!(
        as_i64(&post),
        4,
        "prefix completion is the post-decrement value"
    );
}

#[test]
fn deeply_nested_parentheses_unwrap_to_identifier() {
    // `((x))++` and `--((x))` must behave exactly like `(x)++` / `--(x)`.
    assert_eq!(as_i64(&run("var x = 10; ((x))++; x")), 11);
    assert_eq!(as_i64(&run("var x = 10; ((x))--; x")), 9);
    assert_eq!(as_i64(&run("var x = 10; ++((x)); x")), 11);
    assert_eq!(as_i64(&run("var x = 10; --((x)); x")), 9);
}

#[test]
fn parenthesized_update_matches_unparenthesized_semantics() {
    // The completion value AND the stored value must be identical whether or
    // not the target is wrapped in parentheses.
    for (op, start) in [("++", 5i64), ("--", 5i64)] {
        let bare_stored = run(&format!("var x = {}; {}x; x", start, op));
        let paren_stored = run(&format!("var x = {}; {}(x); x", start, op));
        assert_eq!(as_i64(&bare_stored), as_i64(&paren_stored));
    }
}
