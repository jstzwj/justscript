//! Regression tests for the nullish coalescing operator `??`.
//!
//! Background: an earlier compiler build evaluated both operands of `??` and
//! then dropped one with `Pop`, which corrupted the operand stack inside a
//! computed class key (`vm: computed class key target is not a function`). The
//! compiler now emits real short-circuit control flow that leaves exactly one
//! completion value on the stack on both branches:
//!
//! ```text
//! evaluate lhs
//! Dup
//! JumpIfNullish rhs   ; consumes the duplicate
//! Jump done           ; non-nullish: lhs is the result
//! rhs:
//!   Pop               ; discard the original nullish lhs
//!   evaluate rhs
//! done:
//! ```
//!
//! These tests pin both the structural lowering and the runtime semantics.

use js_bytecode::{compile_program, verify_module, Instruction, Opcode};
use js_runtime::{Value, ValueData};
use js_vm::Interpreter;

/// Parse, compile, verify, and execute a script, returning the completion value.
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

// ---------------------------------------------------------------------------
// Task 1 / 2a: bytecode shape for `??` preserves stack balance on both branches.
// ---------------------------------------------------------------------------

/// The compiled bytecode must match the canonical short-circuit shape with a
/// `Dup` before the test and a `Pop` only on the nullish branch. This is a
/// direct regression guard for the stack-balance bug that broke computed class
/// keys.
#[test]
fn nullish_bytecode_shape_matches_canonical_lowering() {
    let prog = js_parser::parse("null ?? 7").expect("parse");
    let module = compile_program(&prog).expect("compile");
    let code: Vec<Instruction> = module.main.code.clone();

    let mut it = code.iter().enumerate();
    let mut dup_at = None;
    let mut jn_at = None;
    let mut jmp_at = None;
    let mut pop_at = None;
    while let Some((pc, ins)) = it.next() {
        match ins.op {
            Opcode::Dup => dup_at = Some(pc),
            Opcode::JumpIfNullish => jn_at = Some((pc, ins.operand)),
            Opcode::Jump => jmp_at = Some((pc, ins.operand)),
            Opcode::Pop => pop_at = Some(pc),
            _ => {}
        }
    }

    let dup = dup_at.expect("Dup must precede the test");
    let (jn, jn_target) = jn_at.expect("JumpIfNullish must implement the test");
    let (jmp, done_target) = jmp_at.expect("unconditional Jump must skip the rhs");
    let pop = pop_at.expect("Pop must discard the lhs on the nullish branch");

    // Dup immediately precedes JumpIfNullish.
    assert_eq!(jn, dup + 1, "Dup must come right before JumpIfNullish");
    // The unconditional Jump immediately follows the test.
    assert_eq!(jmp, jn + 1, "Jump must come right after JumpIfNullish");
    // The rhs branch begins where the test jumps: Pop discards the lhs there.
    assert_eq!(jn_target, pop as u16, "JumpIfNullish must target the Pop");
    // The fall-through (non-nullish) Jump skips over the rhs to `done`.
    assert!(
        done_target as usize > pop,
        "non-nullish Jump must skip the rhs (Pop+evaluate) to reach done"
    );
    assert!(
        done_target as usize <= code.len(),
        "Jump target must be a valid pc"
    );
}

// ---------------------------------------------------------------------------
// Task 2b: non-nullish LHS must NOT evaluate the RHS (short-circuit).
// ---------------------------------------------------------------------------

#[test]
fn non_nullish_lhs_does_not_evaluate_rhs() {
    // LHS is a truthy number; the RHS is a side-effecting call. The body
    // returns the call counter so we can prove the RHS never ran.
    let src = "var calls = 0; function f() { calls = calls + 1; return 9; } 5 ?? f(); calls";
    assert_eq!(
        as_i64(&run(src)),
        0,
        "RHS must not run when LHS is non-nullish"
    );

    // Also cover `0`, `""`, `false`, and `NaN` — these are falsy but NOT
    // nullish, so `??` must still short-circuit and keep them.
    for lhs in ["0", "\"\"", "false", "NaN"] {
        let src = format!(
            "var calls = 0; function f() {{ calls = calls + 1; return 9; }} ({}) ?? f(); calls",
            lhs
        );
        assert_eq!(
            as_i64(&run(&src)),
            0,
            "RHS must not run for falsy-but-non-nullish LHS ({:?})",
            lhs
        );
    }
}

// ---------------------------------------------------------------------------
// Task 2c: null / undefined LHS evaluates the RHS exactly once.
// ---------------------------------------------------------------------------

#[test]
fn null_lhs_evaluates_rhs_exactly_once() {
    for lhs in ["null", "undefined"] {
        let src = format!(
            "var calls = 0; function f() {{ calls = calls + 1; return 42; }} ({}) ?? f(); calls",
            lhs
        );
        assert_eq!(
            as_i64(&run(&src)),
            1,
            "RHS must run exactly once for nullish LHS ({:?})",
            lhs
        );
    }
}

#[test]
fn nullish_completion_is_lhs_when_non_nullish_rhs_when_nullish() {
    // Non-nullish: completion is the LHS value (5), not the RHS.
    assert_eq!(as_i64(&run("5 ?? 9")), 5);
    // Null LHS: completion is the RHS.
    assert_eq!(as_i64(&run("null ?? 9")), 9);
    // Undefined LHS: completion is the RHS.
    assert_eq!(as_i64(&run("undefined ?? 9")), 9);
}

// ---------------------------------------------------------------------------
// Task 2d: nested `??` stays stack balanced (a ?? b ?? c).
// ---------------------------------------------------------------------------

#[test]
fn nested_coalesce_is_stack_balanced() {
    // First non-nullish wins: undefined -> null -> 9.
    assert_eq!(as_i64(&run("undefined ?? null ?? 9")), 9);
    // The first operand is the result when non-nullish.
    assert_eq!(as_i64(&run("1 ?? 2 ?? 3")), 1);
    // A null in the middle falls through to the last operand.
    assert_eq!(as_i64(&run("null ?? null ?? 7")), 7);

    // Side-effect accounting across a nested chain: each RHS that runs must run
    // at most once, and the chain must stop at the first non-nullish value.
    let src = "var log = 0; \
               function mark(n) { log = log * 10 + n; return n === 2 ? null : n; } \
               mark(1) ?? mark(2) ?? mark(3); \
               log";
    // mark(1)=1 (non-nullish) -> short-circuit, log == 1.
    assert_eq!(as_i64(&run(src)), 1);

    let src2 = "var log = 0; \
                function mark(n) { log = log * 10 + n; return n <= 2 ? null : n; } \
                mark(1) ?? mark(2) ?? mark(3); \
                log";
    // mark(1)=null, mark(2)=null, mark(3)=3 -> log == 123.
    assert_eq!(as_i64(&run(src2)), 123);
}

// ---------------------------------------------------------------------------
// Task 2e: a computed class key using `??` evaluates the key once and leaves
// the class constructor intact.
// ---------------------------------------------------------------------------

#[test]
fn computed_class_key_with_coalesce_evaluates_once_and_keeps_constructor() {
    // The key expression is `base ?? "computed"`. The key is computed exactly
    // once, the resulting method is installed, and `new C()` still works.
    let src = r#"
        var keyCalls = 0;
        function recordKey() { keyCalls = keyCalls + 1; return "k"; }
        var base = null;
        class C {
            [base ?? recordKey()]() { return 42; }
            constructor() { this.field = 7; }
        }
        var c = new C();
        var result = c.k();
        result + c.field + keyCalls
    "#;
    assert_eq!(as_i64(&run(src)), 42 + 7 + 1);

    // Non-nullish base: the RHS of `??` must not run, and the base string is
    // used as the key.
    let src2 = r#"
        var calls = 0;
        function boom() { calls = calls + 1; return "never"; }
        class C { ["m" ?? boom()]() { return 5; } }
        var r = new C().m();
        r + "_" + calls
    "#;
    let v = run(src2);
    match v.data() {
        ValueData::String(s) => assert_eq!(s.as_str(), "5_0"),
        other => panic!("expected \"5_0\", got {:?}", other),
    }
}

#[test]
fn class_with_coalesce_key_is_callable_and_constructible() {
    // A class whose only member uses a `??` computed key must still produce a
    // working constructor and a callable method.
    let src = r#"
        class C {
            [null ?? "compute"]() { return 99; }
        }
        new C().compute()
    "#;
    assert_eq!(as_i64(&run(src)), 99);
}
