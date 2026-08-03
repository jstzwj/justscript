//! Regression tests for function parameter frame layout.
//!
//! Background: the bytecode verifier rejects a function whose `param_count`
//! exceeds `locals.slot_count()` (`parameter count N exceeds local slot
//! count M`). The compiler now reserves one positional frame slot per formal
//! parameter before allocating any named local binding, and the lexical name
//! map may point multiple names (or a duplicate sloppy parameter name) at
//! those slots. For a duplicate non-strict parameter, the body binding must
//! observe the LAST argument supplied for that name.
//!
//! Strict-mode and non-simple (default/rest/destructure) duplicate parameters
//! remain parser early errors and never reach the compiler.

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

/// Every bytecode function must satisfy the verifier's frame invariant:
/// `param_count <= locals.slot_count()`. The buggy compiler allocated a named
/// slot for each parameter *before* bumping `param_count`, which left the
/// counts out of step. This walks every compiled function and re-checks the
/// invariant directly.
#[test]
fn every_function_satisfies_param_count_invariant() {
    let src = r#"
        function noParams() { return 1; }
        function one(a) { return a; }
        function two(a, b) { return a + b; }
        function dup(a, a) { return a; }
        function mixed(a, b, a) { return a; }
        function withLocal(a) { var x = 10; return a + x; }
        function expression(a, b) { return a * b; }
        var arrow = (a, b) => a + b;
        class C { constructor(x, y) { this.sum = x + y; } }
    "#;
    let prog = js_parser::parse(src).expect("parse");
    let module = compile_program(&prog).expect("compile");

    let check = |id: usize, f: &js_bytecode::BytecodeFunction| {
        assert!(
            f.param_count <= f.locals.slot_count(),
            "function {}: param_count {} exceeds slot_count {}",
            id,
            f.param_count,
            f.locals.slot_count(),
        );
    };
    check(0, &module.main);
    for (i, f) in module.functions.iter().enumerate() {
        check(i + 1, f);
    }
    // The full set must verify end-to-end.
    verify_module(&module).expect("verify");
}

// ---------------------------------------------------------------------------
// Task 4 / 5: duplicate sloppy parameters observe the LAST argument.
// ---------------------------------------------------------------------------

#[test]
fn duplicate_param_in_declaration_observes_last_argument() {
    // `function f(a, a)` — the body binding `a` resolves to the second slot,
    // so f(1, 2) returns 2 and f() returns undefined.
    assert_eq!(as_i64(&run("function f(a, a) { return a; } f(1, 2)")), 2);
    // Supplying only one argument: the second slot is undefined, and the body
    // binding still points at it.
    let v = run("function f(a, a) { return a; } f(1)");
    assert!(
        v.is_undefined(),
        "f(1) with dup param -> undefined, got {:?}",
        v
    );
}

#[test]
fn duplicate_param_in_expression_observes_last_argument() {
    let src = "var f = function(a, a) { return a; }; f(10, 20)";
    assert_eq!(as_i64(&run(src)), 20);
}

#[test]
fn duplicate_param_in_method_is_a_parser_early_error() {
    // Class bodies are implicitly strict, so duplicate parameter names in a
    // method or constructor are a SyntaxError at parse time — they never reach
    // the compiler. This is the correct ECMA-262 behavior, not a workaround.
    let cases = [
        "class C { constructor(a, a) {} }",
        "class C { m(a, a) {} }",
        "({ m(a, a) {} })",
        "({ constructor(a, a) {} })",
    ];
    for src in cases {
        let res = js_parser::parse(src);
        assert!(
            res.is_err(),
            "duplicate parameters in a strict/class body must be rejected by the parser: {:?}",
            src
        );
    }
}

#[test]
fn duplicate_param_in_closure_observes_last_argument() {
    // The duplicate binding must survive closure capture: an inner function
    // reading the enclosing `a` sees the last argument.
    let src = r#"
        function outer(a, a) {
            return function () { return a; }();
        }
        outer(100, 200)
    "#;
    assert_eq!(as_i64(&run(src)), 200);
}

#[test]
fn duplicate_param_three_wide_observes_last_argument() {
    // `function f(a, b, a)` — the first positional slot holds the first `a`
    // argument; the body name `a` is rebound to the third slot.
    assert_eq!(
        as_i64(&run("function f(a, b, a) { return a; } f(1, 2, 3)")),
        3
    );
    // The middle distinct parameter is unaffected.
    assert_eq!(
        as_i64(&run("function f(a, b, a) { return b; } f(1, 2, 3)")),
        2
    );
}

// ---------------------------------------------------------------------------
// Task 5: strict-mode and non-simple duplicate parameters are parser early
// errors. They must NOT reach the compiler; do not paper over them here.
// ---------------------------------------------------------------------------

#[test]
fn strict_mode_duplicate_params_are_parser_early_errors() {
    // A `'use strict'` directive in the body makes duplicate simple params a
    // SyntaxError at parse time.
    let res = js_parser::parse("function f(a, a) { 'use strict'; return a; }");
    assert!(
        res.is_err(),
        "strict-mode duplicate parameters must be rejected by the parser"
    );
    // Likewise for a function expression.
    let res = js_parser::parse("var f = function(a, a) { 'use strict'; };");
    assert!(
        res.is_err(),
        "strict-mode duplicate parameters in an expression must be rejected"
    );
}

#[test]
fn non_simple_duplicate_params_are_parser_early_errors() {
    // Any non-simple parameter (default, rest, destructuring) makes duplicate
    // parameter names a SyntaxError regardless of strict mode.
    let cases = [
        "function f(a, a = 1) {}",
        "function f(a, a, ...rest) {}",
        "function f(a, a, { b }) {}",
        "function f(a = 1, a) {}",
    ];
    for src in cases {
        let res = js_parser::parse(src);
        assert!(
            res.is_err(),
            "non-simple duplicate parameters must be rejected by the parser: {:?}",
            src
        );
    }
}
