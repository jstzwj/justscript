//! Milestone-2 end-to-end: objects, arrays, member access, update operators,
//! `for`/`do-while`/`switch`, compound assignment, logical short-circuit,
//! template literals, closures, arrow `this`, and classes (OOP) — all executed
//! through the interpreter pipeline.

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
fn for_loop_sum() {
    // var s = 0; for (var i = 0; i < 5; i++) { s += i; } s  ==>  0+1+2+3+4 = 10
    match run("var s = 0; for (var i = 0; i < 5; i++) { s += i; } s") {
        ValueData::Integer(10) => {}
        v => panic!("expected Integer(10), got {:?}", v),
    }
}

#[test]
fn postfix_and_prefix_update() {
    // var a = 5; var b = a++; var c = ++a;  a==7, b==5, c==7
    match run("var a = 5; var b = a++; var c = ++a; c") {
        ValueData::Integer(7) => {}
        v => panic!("expected Integer(7), got {:?}", v),
    }
}

#[test]
fn object_literal_and_member() {
    // var o = {x: 1, y: 2}; o.x + o.y  ==>  3
    match run("var o = {x: 1, y: 2}; o.x + o.y") {
        ValueData::Integer(3) => {}
        v => panic!("expected Integer(3), got {:?}", v),
    }
}

#[test]
fn member_assignment_and_compound() {
    // var o = {n: 10}; o.n += 5; o.n  ==>  15
    match run("var o = {n: 10}; o.n += 5; o.n") {
        ValueData::Integer(15) => {}
        v => panic!("expected Integer(15), got {:?}", v),
    }
}

#[test]
fn array_index_and_length() {
    // var a = [10, 20, 30]; a[1] + a.length  ==>  20 + 3 = 23
    match run("var a = [10, 20, 30]; a[1] + a.length") {
        ValueData::Integer(23) => {}
        v => panic!("expected Integer(23), got {:?}", v),
    }
}

#[test]
fn logical_short_circuit() {
    // 0 && unreachable-call expression; 1 || 2
    match run("1 || 2") {
        ValueData::Integer(1) => {}
        v => panic!("expected Integer(1), got {:?}", v),
    }
    match run("0 && 7") {
        ValueData::Integer(0) => {}
        v => panic!("expected Integer(0), got {:?}", v),
    }
}

#[test]
fn switch_statement() {
    // switch on 2 → case 2 returns 20
    match run("var r = 0; switch (2) { case 1: r = 10; break; case 2: r = 20; break; default: r = 99; } r") {
        ValueData::Integer(20) => {}
        v => panic!("expected Integer(20), got {:?}", v),
    }
}

#[test]
fn do_while_loop() {
    // var i = 0; var s = 0; do { s += i; i++; } while (i < 4); s  ==>  6
    match run("var i = 0; var s = 0; do { s += i; i++; } while (i < 4); s") {
        ValueData::Integer(6) => {}
        v => panic!("expected Integer(6), got {:?}", v),
    }
}

#[test]
fn template_literal_concat() {
    // `sum is ${1 + 2}`  ==>  "sum is 3"
    match run("var x = 3; `x=${x}`") {
        ValueData::String(s) => assert_eq!(s.as_str(), "x=3"),
        v => panic!("expected String \"x=3\", got {:?}", v),
    }
}

#[test]
fn continue_in_for_loop() {
    // sum of 0..5 skipping 3  ==>  0+1+2+4 = 7
    match run("var s = 0; for (var i = 0; i < 5; i++) { if (i == 3) continue; s += i; } s") {
        ValueData::Integer(7) => {}
        v => panic!("expected Integer(7), got {:?}", v),
    }
}

#[test]
fn nested_function_closure_free_globals() {
    // function add(a,b){return a+b} add(2,3) via globals
    match run("function add(a, b) { return a + b } add(2, 3)") {
        ValueData::Integer(5) => {}
        v => panic!("expected Integer(5), got {:?}", v),
    }
}

// ---- closures ----

#[test]
fn closure_captures_parameter() {
    // makeAdder(10)(3) ==> 13
    match run("function makeAdder(x){ return function(y){ return x + y } } makeAdder(10)(3)") {
        ValueData::Integer(13) => {}
        v => panic!("expected Integer(13), got {:?}", v),
    }
}

#[test]
fn closure_mutates_captured_var() {
    // counter: each call increments a shared captured `c`.
    match run(
        "function mk(){ var c = 0; return function(){ c = c + 1; return c } } \
         var n = mk(); n(); n(); n()",
    ) {
        ValueData::Integer(3) => {}
        v => panic!("expected Integer(3), got {:?}", v),
    }
}

#[test]
fn arrow_curry() {
    match run("var adder = x => y => x + y; adder(10)(3)") {
        ValueData::Integer(13) => {}
        v => panic!("expected Integer(13), got {:?}", v),
    }
}

// ---- `this` ----

#[test]
fn method_this_binding() {
    // obj.m() binds this = obj.
    match run("var o = { x: 10, getX: function(){ return this.x } }; o.getX()") {
        ValueData::Integer(10) => {}
        v => panic!("expected Integer(10), got {:?}", v),
    }
}

#[test]
fn arrow_inherits_this() {
    // Arrow inside a method captures the method's `this`.
    match run("var o = { x: 5, get: function(){ var f = () => this.x; return f() } }; o.get()") {
        ValueData::Integer(5) => {}
        v => panic!("expected Integer(5), got {:?}", v),
    }
}

// ---- classes (OOP) ----

#[test]
fn class_constructor_and_method() {
    match run(
        "class Point { constructor(x, y) { this.x = x; this.y = y; } \
         sum() { return this.x + this.y } } \
         new Point(3, 4).sum()",
    ) {
        ValueData::Integer(7) => {}
        v => panic!("expected Integer(7), got {:?}", v),
    }
}

#[test]
fn class_field_initializers() {
    match run("class C { x = 10; getX(){ return this.x } } new C().getX()") {
        ValueData::Integer(10) => {}
        v => panic!("expected Integer(10), got {:?}", v),
    }
}

#[test]
fn class_instance_state_independent() {
    // Two instances carry independent state.
    match run(
        "class Box { constructor(v){ this.v = v } bump(){ this.v = this.v + 1; return this.v } } \
         var a = new Box(0); var b = new Box(100); a.bump(); b.bump(); b.bump(); a.bump()",
    ) {
        ValueData::Integer(2) => {}
        v => panic!("expected Integer(2), got {:?}", v),
    }
}

// ---- destructuring / iteration ----

#[test]
fn array_destructuring() {
    match run("var [a, b, c] = [1, 2, 3]; a + b + c") {
        ValueData::Integer(6) => {}
        v => panic!("expected Integer(6), got {:?}", v),
    }
}

#[test]
fn object_destructuring() {
    match run("var {x, y} = {x: 10, y: 20}; x + y") {
        ValueData::Integer(30) => {}
        v => panic!("expected Integer(30), got {:?}", v),
    }
}

#[test]
fn for_of_sum() {
    match run("var s = 0; for (var x of [1,2,3,4]) s = s + x; s") {
        ValueData::Integer(10) => {}
        v => panic!("expected Integer(10), got {:?}", v),
    }
}

#[test]
fn for_of_with_destructuring() {
    match run("var s = 0; for (var [a, b] of [[1,2],[3,4]]) s = s + a + b; s") {
        ValueData::Integer(10) => {}
        v => panic!("expected Integer(10), got {:?}", v),
    }
}

#[test]
fn array_spread() {
    match run("var a = [1,2,3]; var b = [0, ...a, 4]; b.length") {
        ValueData::Integer(5) => {}
        v => panic!("expected Integer(5), got {:?}", v),
    }
}

#[test]
fn for_in_keys() {
    // Collect object keys via for-in.
    match run("var n = 0; for (var k in {a:1, b:2, c:3}) n = n + 1; n") {
        ValueData::Integer(3) => {}
        v => panic!("expected Integer(3), got {:?}", v),
    }
}

// ---- generators ----

#[test]
fn generator_basic() {
    match run("function* g(){ yield 1; yield 2 } var it = g(); it.next().value + it.next().value") {
        ValueData::Integer(3) => {}
        v => panic!("expected Integer(3), got {:?}", v),
    }
}

#[test]
fn generator_done() {
    // After the body completes, done is true.
    match run("function* g(){ yield 1 } var it = g(); it.next(); it.next().done") {
        ValueData::Boolean(true) => {}
        v => panic!("expected Boolean(true), got {:?}", v),
    }
}

#[test]
fn generator_next_arg_is_yield_value() {
    // .next(v) sends v as the value of the yield expression.
    match run("function* g(){ var x = yield 1; return x } var it = g(); it.next().value; it.next(10).value") {
        ValueData::Integer(10) => {}
        v => panic!("expected Integer(10), got {:?}", v),
    }
}

#[test]
fn generator_counter() {
    match run("function* counter(){ var i=0; while(true){ yield i; i = i + 1 } } var c = counter(); c.next().value + c.next().value + c.next().value") {
        ValueData::Integer(3) => {}
        v => panic!("expected Integer(3), got {:?}", v),
    }
}

#[test]
fn generator_expression() {
    match run("var g = function*(){ yield 5 }; g().next().value") {
        ValueData::Integer(5) => {}
        v => panic!("expected Integer(5), got {:?}", v),
    }
}

#[test]
fn generator_yield_star_delegates_and_returns() {
    match run(
        "function* g(){ var result = yield *\n[1, 2]; return result } \
         var it = g(); it.next().value * 100 + it.next().value * 10 + (it.next().done ? 1 : 0)",
    ) {
        ValueData::Integer(121) => {}
        v => panic!("expected Integer(121), got {:?}", v),
    }
}

#[test]
fn generator_yield_star_forwards_next_value() {
    match run("function* inner(){ var value = yield 1; return value } \
         function* outer(){ return yield* inner() } \
         var it = outer(); it.next(); it.next(9).value")
    {
        ValueData::Integer(9) => {}
        v => panic!("expected Integer(9), got {:?}", v),
    }
}

#[test]
fn generator_yield_star_forwards_throw_and_return() {
    match run("var iterator = { \
           next: function(){ return { value: 1, done: false } }, \
           throw: function(value){ return { value: value + 1, done: true } }, \
           return: function(value){ return { value: value + 2, done: true } }, \
           [Symbol.iterator]: function(){ return this } \
         }; \
         function* outer(){ return yield* iterator } \
         var thrown = outer(); thrown.next(); var a = thrown.throw(8).value; \
         var returned = outer(); returned.next(); var b = returned.return(8).value; \
         a * 10 + b")
    {
        ValueData::Integer(100) => {}
        v => panic!("expected Integer(100), got {:?}", v),
    }
}

// ---- iterator protocol: for-of / spread over generators ----

#[test]
fn for_of_over_generator() {
    match run("function* g(){ yield 1; yield 2; yield 3 } var s=0; for (var x of g()) s=s+x; s") {
        ValueData::Integer(6) => {}
        v => panic!("expected Integer(6), got {:?}", v),
    }
}

#[test]
fn for_of_generator_with_break() {
    match run("function* c(){ var i=0; while(true){ yield i; i=i+1 } } var s=0; for (var x of c()){ if(x>3) break; s=s+x } s") {
        ValueData::Integer(6) => {}
        v => panic!("expected Integer(6), got {:?}", v),
    }
}

#[test]
fn spread_generator_into_array() {
    match run("function* g(){ yield 1; yield 2; yield 3 } var a = [...g()]; a[0]+a[1]+a[2]") {
        ValueData::Integer(6) => {}
        v => panic!("expected Integer(6), got {:?}", v),
    }
}

#[test]
fn mixed_spread_generator_and_literals() {
    match run("function* g(){ yield 2; yield 3 } var a = [1, ...g(), 4]; a.length") {
        ValueData::Integer(4) => {}
        v => panic!("expected Integer(4), got {:?}", v),
    }
}

// ---- Array / String builtins ----

#[test]
fn array_push_join() {
    match run("var a=[1,2,3]; a.push(4); a.join('-')") {
        ValueData::String(s) => assert_eq!(s.as_str(), "1-2-3-4"),
        v => panic!("got {:?}", v),
    }
}

#[test]
fn array_slice_concat_indexof() {
    assert!(
        matches!(run("[5,4,3,2,1].slice(1,3).join(',')"), ValueData::String(s) if s.as_str()=="4,3")
    );
    assert!(matches!(run("[1,2,3].indexOf(2)"), ValueData::Integer(1)));
    assert!(matches!(
        run("[1,2,3].includes(5)"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(
        run("[1,2].concat([3,4]).length"),
        ValueData::Integer(4)
    ));
}

#[test]
fn string_methods() {
    assert!(matches!(run("'hello'.toUpperCase()"), ValueData::String(s) if s.as_str()=="HELLO"));
    assert!(matches!(run("'  hi  '.trim()"), ValueData::String(s) if s.as_str()=="hi"));
    assert!(matches!(
        run("'a,b,c'.split(',').length"),
        ValueData::Integer(3)
    ));
    assert!(matches!(run("'hello'.slice(1,3)"), ValueData::String(s) if s.as_str()=="el"));
    assert!(matches!(run("'abc'.repeat(3)"), ValueData::String(s) if s.as_str()=="abcabcabc"));
    assert!(matches!(run("'hello'.charAt(1)"), ValueData::String(s) if s.as_str()=="e"));
}

#[test]
fn array_pop_shift() {
    match run("var a=[1,2,3]; var x = a.pop(); x + a.length") {
        ValueData::Integer(5) => {}
        v => panic!("got {:?}", v),
    }
}

// ---- callback Array methods (native→JS sub-dispatch) ----

#[test]
fn array_map() {
    assert!(
        matches!(run("[1,2,3].map(x => x * 2).join(',')"), ValueData::String(s) if s.as_str()=="2,4,6")
    );
}

#[test]
fn array_filter() {
    assert!(
        matches!(run("[1,2,3,4].filter(x => x % 2 == 0).join(',')"), ValueData::String(s) if s.as_str()=="2,4")
    );
}

#[test]
fn array_reduce() {
    assert!(matches!(
        run("[1,2,3,4].reduce((a, b) => a + b, 0)"),
        ValueData::Integer(10)
    ));
    assert!(matches!(
        run("[1,2,3,4].reduce((a, b) => a + b)"),
        ValueData::Integer(10)
    ));
}

#[test]
fn array_foreach_and_find() {
    assert!(matches!(
        run("var s = 0; [10,20,30].forEach(x => s = s + x); s"),
        ValueData::Integer(60)
    ));
    assert!(matches!(
        run("[1,2,3,4].find(x => x > 2)"),
        ValueData::Integer(3)
    ));
}

#[test]
fn array_some_every() {
    assert!(matches!(
        run("[1,2,3].some(x => x > 2)"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("[1,2,3].every(x => x > 0)"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("[1,2,3].every(x => x > 1)"),
        ValueData::Boolean(false)
    ));
}

#[test]
fn array_map_captures_closure_and_index() {
    // Callback arrow captures an outer var AND uses the index argument.
    assert!(
        matches!(run("var scale = 10; [1,2,3].map((x, i) => x * scale + i).join(',')"), ValueData::String(s) if s.as_str()=="10,21,32")
    );
}

// ---- global builtins ----

#[test]
fn math_max_min() {
    assert!(matches!(
        run("Math.max(3, 7, 2) + Math.min(8, 2)"),
        ValueData::Integer(9)
    ));
    assert!(matches!(run("Math.floor(Math.PI)"), ValueData::Number(_)));
}

#[test]
fn object_keys_values() {
    assert!(matches!(
        run("Object.keys({a:1,b:2}).length"),
        ValueData::Integer(2)
    ));
    assert!(matches!(
        run("Object.values({a:1,b:2}).reduce((x,y)=>x+y,0)"),
        ValueData::Integer(3)
    ));
}

#[test]
fn parse_int_and_coercion() {
    assert!(matches!(
        run("parseInt('42') + parseInt('ff', 16)"),
        ValueData::Integer(297)
    ));
    assert!(matches!(run("Number('3.5')"), ValueData::Number(_)));
    assert!(matches!(run("String(42) + 'x'"), ValueData::String(s) if s.as_str()=="42x"));
    assert!(matches!(run("Boolean(0)"), ValueData::Boolean(false)));
}

#[test]
fn array_isarray_and_json() {
    assert!(matches!(
        run("Array.isArray([1])"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(run("Array.isArray(5)"), ValueData::Boolean(false)));
    assert!(
        matches!(run("JSON.stringify({a: [1,2]})"), ValueData::String(s) if s.contains("\"a\":[1,2]"))
    );
}

#[test]
fn string_array_to_string_join() {
    // Array toString === join(",").
    assert!(matches!(run("String([1,2,3])"), ValueData::String(s) if s.as_str()=="1,2,3"));
}

// ---- more Array/String builtins ----

#[test]
fn array_sort_default_and_comparator() {
    assert!(matches!(run("[3,1,2].sort().join(',')"), ValueData::String(s) if s.as_str()=="1,2,3"));
    assert!(
        matches!(run("[3,1,2].sort((a,b)=>a-b).join(',')"), ValueData::String(s) if s.as_str()=="1,2,3")
    );
    assert!(
        matches!(run("[3,1,2].sort((a,b)=>b-a).join(',')"), ValueData::String(s) if s.as_str()=="3,2,1")
    );
}

#[test]
fn array_flat_fill_at() {
    assert!(
        matches!(run("[1,[2,[3]]].flat().join(',')"), ValueData::String(s) if s.as_str()=="1,2,3")
    );
    assert!(
        matches!(run("[1,2,3].fill(7).join(',')"), ValueData::String(s) if s.as_str()=="7,7,7")
    );
    assert!(matches!(run("[10,20,30].at(-1)"), ValueData::Integer(30)));
}

#[test]
fn string_pad_starts_replace() {
    assert!(matches!(run("'5'.padStart(3,'0')"), ValueData::String(s) if s.as_str()=="005"));
    assert!(matches!(run("'hi'.padEnd(4,'.')"), ValueData::String(s) if s.as_str()=="hi.."));
    assert!(matches!(
        run("'hello'.startsWith('he')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("'hello'.endsWith('lo')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(run("'hello'.replace('l','L')"), ValueData::String(s) if s.as_str()=="heLlo"));
    assert!(matches!(run("'abc'.at(-1)"), ValueData::String(s) if s.as_str()=="c"));
}

// ---- try / catch / finally / throw ----

#[test]
fn throw_catch_binding() {
    assert!(matches!(
        run("var r; try { throw 42 } catch(e) { r = e } r"),
        ValueData::Integer(42)
    ));
}

#[test]
fn try_normal_no_catch() {
    assert!(matches!(
        run("var r=0; try { r=1 } catch(e){ r=2 } r"),
        ValueData::Integer(1)
    ));
}

#[test]
fn throw_propagates_across_call() {
    assert!(
        matches!(run("var r; function f(){ throw 'boom' } try { f() } catch(e){ r=e } r"), ValueData::String(s) if s.as_str()=="boom")
    );
}

#[test]
fn throw_in_callback_caught_outside() {
    // map's native drives the callback via call_value; its throw propagates out.
    assert!(
        matches!(run("var r; try { [1,2].map(x => { throw 'inmap' }) } catch(e){ r=e } r"), ValueData::String(s) if s.as_str()=="inmap")
    );
}

#[test]
fn try_finally_runs_and_rethrows() {
    // Inner try/finally re-throws after running finally; outer catch catches it.
    assert!(
        matches!(run("var s=''; try { try { throw 'a' } finally { s+='F' } } catch(e){ s+=e } s"), ValueData::String(s) if s.as_str()=="Fa")
    );
}

#[test]
fn catch_then_finally() {
    assert!(
        matches!(run("var s=''; try { throw 1 } catch(e){ s+='C' } finally { s+='F' } s"), ValueData::String(s) if s.as_str()=="CF")
    );
}

#[test]
fn nested_catch() {
    assert!(
        matches!(run("var r; try { try { throw 'x' } catch(e){ throw 'y' } } catch(e){ r=e } r"), ValueData::String(s) if s.as_str()=="y")
    );
}

// ---- Error + instanceof ----

#[test]
fn throw_new_error_catch_instanceof() {
    assert!(matches!(
        run("var r; try { throw new TypeError('bad') } catch(e) { r = e instanceof TypeError } r"),
        ValueData::Boolean(true)
    ));
}

#[test]
fn instanceof_error_base() {
    assert!(matches!(
        run("var r; try { throw new RangeError('oob') } catch(e) { r = e instanceof Error } r"),
        ValueData::Boolean(true)
    ));
}

#[test]
fn instanceof_wrong_type() {
    assert!(matches!(
        run("var r; try { throw new TypeError('x') } catch(e) { r = e instanceof RangeError } r"),
        ValueData::Boolean(false)
    ));
}

#[test]
fn error_message_and_name() {
    assert!(
        matches!(run("var r; try { throw new SyntaxError('oops') } catch(e) { r = e.name + ':' + e.message } r"),
        ValueData::String(s) if s.as_str()=="SyntaxError:oops")
    );
}

#[test]
fn error_without_new() {
    assert!(
        matches!(run("var e = Error('hi'); e.name + ':' + e.message"),
        ValueData::String(s) if s.as_str()=="Error:hi")
    );
}

// ---- JSON.parse + Number/String methods ----

#[test]
fn json_parse_roundtrip() {
    assert!(matches!(
        run("var o = JSON.parse('{\"a\":1,\"b\":[2,3]}'); o.a + o.b[0] + o.b[1]"),
        ValueData::Integer(6)
    ));
}

#[test]
fn json_parse_stringify_roundtrip() {
    assert!(
        matches!(run("var s = JSON.stringify({x: 1, y: 'z'}); var o = JSON.parse(s); o.x + o.y"),
        ValueData::String(ref s) if s.as_str() == "1z")
    );
}

#[test]
fn number_tofixed() {
    assert!(matches!(run("(3.14159).toFixed(2)"), ValueData::String(s) if s.as_str()=="3.14"));
    assert!(matches!(run("(0).toFixed(3)"), ValueData::String(s) if s.as_str()=="0.000"));
}

#[test]
fn number_statics() {
    assert!(matches!(
        run("Number.isInteger(42)"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("Number.isInteger(3.14)"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(run("Number.isNaN(NaN)"), ValueData::Boolean(true)));
}

#[test]
fn string_extras() {
    assert!(matches!(run("String.fromCharCode(65,66)"), ValueData::String(s) if s.as_str()=="AB"));
    assert!(matches!(run("'  hi  '.trimStart()"), ValueData::String(s) if s.as_str()=="hi  "));
    assert!(matches!(run("'  hi  '.trimEnd()"), ValueData::String(s) if s.as_str()=="  hi"));
}

// ---- regex matching ----

#[test]
fn regex_test() {
    assert!(matches!(
        run("/abc/.test('xabcx')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/abc/.test('xyz')"),
        ValueData::Boolean(false)
    ));
}

#[test]
fn regex_digit_class() {
    assert!(matches!(
        run("/\\d+/.test('a1b')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/\\d+/.test('abc')"),
        ValueData::Boolean(false)
    ));
}

#[test]
fn regex_exec_captures() {
    assert!(
        matches!(run("var m = /(\\d+)-(\\d+)/.exec('12-34'); m[0] + ',' + m[1] + ',' + m[2]"),
        ValueData::String(s) if s.as_str()=="12-34,12,34")
    );
}

#[test]
fn string_match() {
    assert!(
        matches!(run("'hello world'.match(/\\w+/)[0]"), ValueData::String(s) if s.as_str()=="hello")
    );
}

#[test]
fn string_search() {
    assert!(matches!(
        run("'hello world'.search(/world/)"),
        ValueData::Integer(6)
    ));
}

#[test]
fn regex_case_insensitive() {
    assert!(matches!(
        run("/hello/i.test('HELLO')"),
        ValueData::Boolean(true)
    ));
}

#[test]
fn regex_flags() {
    assert!(matches!(run("/abc/gim.flags"), ValueData::String(s) if s.as_str()=="gim"));
    assert!(matches!(run("/abc/g.global"), ValueData::Boolean(true)));
}

#[test]
fn regex_bounded_quantifier() {
    // {n}, {n,m}, {n,} — still on the linear Pike-VM path.
    assert!(matches!(
        run("/a{3}/.test('aaa')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/a{3}/.test('aa')"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(
        run("/a{2,4}/.test('aaa')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/a{2,4}/.test('a')"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(
        run("/a{2,}/.test('aaaaa')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/a{2,}/.test('a')"),
        ValueData::Boolean(false)
    ));
    // {n,m} applies to a group, with capture.
    assert!(matches!(
        run("var m = /(ab){2}/.exec('abab'); m[1]"),
        ValueData::String(s) if s.as_str() == "ab"
    ));
}

#[test]
fn regex_lookahead_lookbehind() {
    // lookahead: consume 'a' only if followed by 'b'
    assert!(matches!(
        run("/a(?=b)/.test('ab')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/a(?=b)/.test('ac')"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(
        run("/a(?!b)/.test('ac')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/a(?!b)/.test('ab')"),
        ValueData::Boolean(false)
    ));
    // lookbehind
    assert!(matches!(
        run("/(?<=a)b/.test('ab')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/(?<=a)b/.test('xb')"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(
        run("/(?<!a)b/.test('xb')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/(?<!a)b/.test('ab')"),
        ValueData::Boolean(false)
    ));
    // capture inside lookahead is preserved
    assert!(matches!(
        run("/(?=(\\d+))/.exec('x42')[1]"),
        ValueData::String(s) if s.as_str() == "42"
    ));
}

#[test]
fn regex_backreferences() {
    // numeric backreference: a repeated word
    assert!(matches!(
        run("/(\\w+) \\1/.test('hi hi')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/(\\w+) \\1/.test('hi ho')"),
        ValueData::Boolean(false)
    ));
    // named backreference
    assert!(matches!(
        run("/(?<w>\\w+)-\\k<w>/.test('go-go')"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("/(?<w>\\w+)-\\k<w>/.test('go-stop')"),
        ValueData::Boolean(false)
    ));
}

#[test]
fn regex_replace_string_substitution() {
    // global regex replace
    assert!(matches!(
        run("'hello'.replace(/l/g, 'L')"),
        ValueData::String(s) if s.as_str() == "heLLo"
    ));
    // $1 group substitution
    assert!(matches!(
        run("'a1b2'.replace(/(\\d)/g, '[$1]')"),
        ValueData::String(s) if s.as_str() == "a[1]b[2]"
    ));
    // $<name> named-group substitution
    assert!(matches!(
        run("'a1b2'.replace(/(?<d>\\d)/g, '<$<d>>')"),
        ValueData::String(s) if s.as_str() == "a<1>b<2>"
    ));
    // $& whole-match
    assert!(matches!(
        run("'cat'.replace(/a/, '_$&_')"),
        ValueData::String(s) if s.as_str() == "c_a_t"
    ));
}

#[test]
fn regex_replace_callback() {
    // function replacement (regex, global)
    assert!(matches!(
        run("'a1b2'.replace(/\\d/g, function(m){ return '[' + m + ']'; })"),
        ValueData::String(s) if s.as_str() == "a[1]b[2]"
    ));
    // arrow with capture groups, offset, and full string
    assert!(matches!(
        run("'x42y'.replace(/(\\d)(\\d)/, (a, p1, p2, off) => p1 + p2 + '@' + off)"),
        ValueData::String(s) if s.as_str() == "x42@1y"
    ));
    // function replacement on a string pattern (first occurrence)
    assert!(matches!(
        run("'hello world'.replace('world', w => w.toUpperCase())"),
        ValueData::String(s) if s.as_str() == "hello WORLD"
    ));
}

#[test]
fn unsigned_shift_has_distinct_semantics() {
    assert!(matches!(
        run("-1 >>> 0"),
        ValueData::Number(n) if n == 4_294_967_295.0
    ));
}

#[test]
fn in_tests_property_presence_not_value() {
    assert!(matches!(
        run("var o = {present: undefined}; 'present' in o"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("var o = {}; 'missing' in o"),
        ValueData::Boolean(false)
    ));
}

#[test]
fn void_preserves_side_effects_and_returns_undefined() {
    assert!(matches!(
        run("var n = 0; var result = void (n = 3); n === 3 && result === undefined"),
        ValueData::Boolean(true)
    ));
}

#[test]
fn delete_distinguishes_properties_and_bindings() {
    assert!(matches!(
        run("var o = {x: 1}; var deleted = delete o.x; deleted && !('x' in o)"),
        ValueData::Boolean(true)
    ));
    assert!(matches!(
        run("var bound = 1; delete bound"),
        ValueData::Boolean(false)
    ));
    assert!(matches!(
        run("delete unresolvableName"),
        ValueData::Boolean(true)
    ));
}

#[test]
fn static_class_elements_run_with_constructor_as_this() {
    assert!(matches!(
        run(r#"
class Counter {
    static value = 40;
    static { this.value += 1; }
    static answer() { return this.value + 1; }
}
Counter.answer();
"#),
        ValueData::Integer(42)
    ));
}

#[test]
fn private_fields_are_branded_and_support_get_set_and_in() {
    assert!(matches!(
        run(r#"
class Counter {
    #value = 40;
    increment() { this.#value += 2; return this.#value; }
    has(value) { return #value in value; }
}
let counter = new Counter();
counter.increment() === 42 && counter.has(counter) && !counter.has({});
"#),
        ValueData::Boolean(true)
    ));
}

#[test]
fn static_private_fields_live_on_the_constructor() {
    assert!(matches!(
        run(r#"
class Counter {
    static #value = 40;
    static increment() { this.#value += 2; return this.#value; }
}
Counter.increment();
"#),
        ValueData::Integer(42)
    ));
}

#[test]
fn equal_private_spellings_in_distinct_classes_do_not_share_a_brand() {
    let mut engine = Engine::default_interpreter();
    let error = engine
        .run(
            r#"
class A { #value = 1; read(other) { return other.#value; } }
class B { #value = 2; }
new A().read(new B());
"#,
        )
        .unwrap_err();
    let js_engine::EngineError::Exception(exception) = error else {
        panic!("expected a JavaScript exception, found {error:?}");
    };
    assert_eq!(exception.value.error_name().as_deref(), Some("TypeError"));
}

#[test]
fn each_class_evaluation_allocates_a_fresh_private_brand() {
    assert!(matches!(
        run(r#"
function make() {
    class C {
        #method() { return 42; }
        read(other) { return other.#method(); }
    }
    return new C();
}
let first = make();
let second = make();
let isolated = false;
try { first.read(second); } catch (error) { isolated = error instanceof TypeError; }
first.read(first) === 42 && second.read(second) === 42 && isolated;
"#),
        ValueData::Boolean(true)
    ));
}

#[test]
fn derived_construction_forwards_arguments_and_rejects_duplicate_private_brand() {
    assert!(matches!(
        run(r#"
class Base { constructor(value) { return value; } }
class Derived extends Base { #method() {} }
let object = {};
new Derived(object);
let duplicateRejected = false;
try { new Derived(object); } catch (error) { duplicateRejected = error instanceof TypeError; }
duplicateRejected;
"#),
        ValueData::Boolean(true)
    ));
}

#[test]
fn multi_level_derived_construction_initializes_each_class_once_in_order() {
    assert!(matches!(
        run(r#"
let order = [];
class A { #a = order.push("A"); hasA() { return #a in this; } }
class B extends A { #b = order.push("B"); hasB() { return #b in this; } }
class C extends B { #c = order.push("C"); hasC() { return #c in this; } }
let value = new C();
order.join("") === "ABC" && value.hasA() && value.hasB() && value.hasC();
"#),
        ValueData::Boolean(true)
    ));
}

#[test]
fn derived_fields_run_after_super_and_before_the_remaining_constructor_body() {
    assert!(matches!(
        run(r#"
let order = [];
class Base { field = order.push("base"); }
class Derived extends Base {
    field = order.push("derived");
    constructor() {
        order.push("before");
        super();
        order.push("after");
    }
}
new Derived();
order.join(":");
"#),
        ValueData::String(value) if value.as_str() == "before:base:derived:after"
    ));
}

#[test]
fn class_accessors_use_descriptors_for_reads_and_writes() {
    assert!(matches!(
        run(r#"
let stored;
class C {
    static get value() { return stored; }
    static set value(next) { stored = next; }
}
C.value = 42;
C.value;
"#),
        ValueData::Integer(42)
    ));
}

#[test]
fn anonymous_class_field_functions_receive_field_names() {
    assert!(matches!(
        run(r#"
class C {
    static #private = () => 1;
    static public = function() {};
    static names() { return this.#private.name + ":" + this.public.name; }
}
C.names();
"#),
        ValueData::String(value) if value.as_str() == "#private:public"
    ));
}

#[test]
fn computed_class_keys_are_converted_once_at_definition_time() {
    assert!(matches!(
        run(r#"
let conversions = 0;
let key = { toString() { conversions += 1; return "answer"; } };
class C { [key] = 42; }
let first = new C();
let second = new C();
conversions === 1 && first.answer === 42 && second.answer === 42;
"#),
        ValueData::Boolean(true)
    ));
}

#[test]
fn direct_eval_inherits_lexical_this_and_private_environment() {
    assert!(matches!(
        run(r#"
let observed = false;
class C {
    #value = 42;
    read() {
        return eval("observed = true; () => this.#value");
    }
}
let closure = new C().read();
observed && closure() === 42;
"#),
        ValueData::Boolean(true)
    ));
}

#[test]
fn calling_and_constructing_non_callable_values_throw_type_error() {
    let mut engine = Engine::default_interpreter();
    for source in ["null()", "new (42)()"] {
        let error = engine.run(source).unwrap_err();
        let js_engine::EngineError::Exception(exception) = error else {
            panic!("expected JavaScript exception, found {error:?}");
        };
        assert_eq!(exception.value.error_name().as_deref(), Some("TypeError"));
    }
}
