//! The dispatch loop.
//!
//! [`Interpreter::run_module`] pushes a frame for `<main>` and dispatches
//! instructions until a `Return` unwinds to the top. Function calls push new
//! frames; `Return` pops them and pushes the return value onto the caller's
//! operand stack — so the whole call tree runs inside one flat loop, no Rust
//! recursion.

use crate::frame::{new_cell, CallFrame};
use js_bytecode::{BytecodeFunction, BytecodeModule, Opcode};
use js_diagnostics::DiagResult;
use js_runtime::context::RealmContext;
use js_runtime::value::{GeneratorState, JsFunction, Value, ValueData};
use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

/// Native (Rust-implemented) function ids registered in the VM.
mod native_id {
    pub const GEN_NEXT: u16 = 0;
    pub const GEN_RETURN: u16 = 1;
    pub const GEN_THROW: u16 = 2;
}

/// Hidden property keys on array-iterator objects (source + cursor index).
const IT_SRC: &str = "__it_src";
const IT_IDX: &str = "__it_idx";

/// The result of a native call: either a plain value, or a request to resume a
/// paused generator with `arg` (the value passed to `.next(arg)`).
pub enum NativeResult {
    Value(Value),
    ResumeGenerator(Rc<RefCell<GeneratorState>>, Value),
}

/// Outcome of one [`Interpreter::step`]: keep going, or the running frame
/// returned (the top-level script in `dispatch`).
pub(crate) enum Step {
    More,
    Done(Value),
}

/// A Rust-implemented builtin function. `this` is the call receiver (e.g. the
/// array for `arr.push`); `f` is the callee value (so generator methods can
/// read their bound generator).
pub trait NativeFn {
    fn call(
        &self,
        interp: &mut Interpreter,
        module: &BytecodeModule,
        this: Value,
        f: &JsFunction,
        args: Vec<Value>,
    ) -> Result<NativeResult, InterpError>;
}

struct GenNext;
struct GenReturn;
struct GenThrow;

impl NativeFn for GenThrow {
    fn call(&self, _interp: &mut Interpreter, _module: &BytecodeModule, _this: Value, f: &JsFunction, _args: Vec<Value>) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        gen.borrow_mut().done = true;
        Ok(NativeResult::Value(iter_result(Value::undefined(), true)))
    }
}

impl NativeFn for GenNext {
    fn call(&self, _interp: &mut Interpreter, _module: &BytecodeModule, _this: Value, f: &JsFunction, args: Vec<Value>) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let arg = args.into_iter().next().unwrap_or_else(Value::undefined);
        Ok(NativeResult::ResumeGenerator(gen, arg))
    }
}

impl NativeFn for GenReturn {
    fn call(&self, _interp: &mut Interpreter, _module: &BytecodeModule, _this: Value, f: &JsFunction, args: Vec<Value>) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let value = args.into_iter().next().unwrap_or_else(Value::undefined);
        gen.borrow_mut().done = true;
        Ok(NativeResult::Value(iter_result(value, true)))
    }
}

/// A runtime error surfaced from the interpreter.
#[derive(Debug)]
pub enum InterpError {
    /// A user-visible JavaScript throw.
    Throw(Value),
    /// A VM bug / unimplemented opcode.
    Internal(String),
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterpError::Throw(v) => write!(f, "Uncaught {}", to_string(v)),
            InterpError::Internal(msg) => write!(f, "internal interpreter error: {msg}"),
        }
    }
}

impl std::error::Error for InterpError {}

/// The bytecode interpreter.
pub struct Interpreter {
    ctx: RealmContext,
    frames: Vec<CallFrame>,
    /// Registered native (Rust) builtins, indexed by `JsFunction.native`.
    natives: Vec<Box<dyn NativeFn>>,
    /// A native-call error surfaced to the dispatch loop (which checks it and
    /// unwinds). `None` while no error is pending.
    pending_err: Option<InterpError>,
}

impl Interpreter {
    pub fn new(ctx: RealmContext) -> Interpreter {
        let interp = Interpreter {
            ctx,
            frames: Vec::new(),
            natives: default_natives(),
            pending_err: None,
        };
        // Install the global builtins (console, Math, Object, JSON, parseInt, …).
        {
            let mut realm = interp.ctx.realm.borrow_mut();
            crate::builtins::install_globals(&mut realm.globals);
        }
        interp
    }

    /// Construct an interpreter with a fresh realm.
    pub fn fresh() -> Interpreter {
        Interpreter::new(RealmContext::fresh())
    }

    /// Execute a compiled module's top-level function.
    pub fn run_module(&mut self, module: &BytecodeModule) -> Result<Value, InterpError> {
        let span = module.main.span;
        let frame = CallFrame::new(0, module.main.locals.slot_count(), span);
        self.frames.push(frame);
        self.dispatch(module)
    }

    /// Main dispatch loop: runs instructions until the top-level frame returns.
    /// Exceptions surface as `Err` from [`Self::step`]; [`Self::handle_exception`]
    /// either installs a catch/finally (continue) or propagates (pop frame).
    fn dispatch(&mut self, module: &BytecodeModule) -> Result<Value, InterpError> {
        loop {
            match self.step(module) {
                Ok(Step::More) => {}
                Ok(Step::Done(v)) => return Ok(v),
                Err(e) => match self.handle_exception(e, 0) {
                    Ok(()) => {}
                    Err(e) => return Err(e),
                },
            }
        }
    }

    /// Try to handle an exception: pop frames (down to `stop_depth`) until one
    /// has an active catch/finally to install; if none, propagate the exception
    /// (return `Err`). `dispatch` uses stop_depth 0; `call_value` uses its
    /// caller-depth target so it never touches the suspended caller frame.
    fn handle_exception(&mut self, e: InterpError, stop_depth: usize) -> Result<(), InterpError> {
        let thrown = match e {
            InterpError::Throw(v) => v,
            other => return Err(other), // internal errors always propagate
        };
        while self.frames.len() > stop_depth {
            let handler = self.frames.last_mut().unwrap().try_stack.pop();
            match handler {
                Some(h) => {
                    let frame = self.frames.last_mut().unwrap();
                    if let Some(catch_pc) = h.catch_pc {
                        frame.pc = catch_pc as usize;
                        frame.stack.push(thrown);
                        frame.pending_throw = None; // caught
                    } else if let Some(finally_pc) = h.finally_pc {
                        frame.pc = finally_pc as usize;
                        frame.pending_throw = Some(thrown); // re-raise after finally
                    }
                    return Ok(());
                }
                None => {
                    self.frames.pop(); // no handler here → unwind to caller
                }
            }
        }
        Err(InterpError::Throw(thrown))
    }

    /// One-instruction outcome of [`Self::step`]: keep going, or the running
    /// frame returned `Value` (the top-level script, in [`Self::dispatch`]).
    fn step(&mut self, module: &BytecodeModule) -> Result<Step, InterpError> {
            // A native call may have surfaced an error to unwind the loop.
            if let Some(err) = self.pending_err.take() {
                return Err(err);
            }
            // Fetch + advance the PC without holding a long-lived borrow.
            let ins = {
                let frame = self.frames.last_mut().unwrap();
                let func = func_ref(module, frame.func_index);
                match func.code.get(frame.pc) {
                    Some(ins) => {
                        frame.pc += 1;
                        frame.span = func.span;
                        *ins
                    }
                    None => {
                        // Fell off the end of a function without an explicit
                        // Return — treat as `return undefined`.
                        return Ok(Step::Done(Value::undefined()));
                    }
                }
            };

            match ins.op {
                Opcode::Nop => {}
                Opcode::ArrayPush => {
                    let v = self.top().stack.pop();
                    let arr = self.top().stack.peek().clone();
                    array_append(&arr, v);
                }
                Opcode::ArrayExtend => {
                    let src = self.top().stack.pop();
                    let arr = self.top().stack.peek().clone();
                    let values: Vec<Value> = match src.data() {
                        ValueData::Object(o) => {
                            let b = o.borrow();
                            let len = b
                                .properties
                                .get("length")
                                .and_then(|d| match d {
                                    js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                                    _ => None,
                                })
                                .and_then(|v| match v.data() {
                                    ValueData::Integer(i) => Some(*i as usize),
                                    ValueData::Number(n) => Some(*n as usize),
                                    _ => None,
                                })
                                .unwrap_or(0);
                            (0..len)
                                .filter_map(|i| {
                                    b.properties.get(&i.to_string()).and_then(|d| match d {
                                        js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                                        _ => None,
                                    })
                                })
                                .collect()
                        }
                        ValueData::String(s) => s.as_str().chars().map(|c| Value::string(c.to_string())).collect(),
                        _ => Vec::new(),
                    };
                    for v in values {
                        array_append(&arr, v);
                    }
                }
                Opcode::GetIterator => {
                    let v = self.top().stack.pop();
                    self.top().stack.push(make_iterator(&v));
                }
                Opcode::IterNext => {
                    let it = self.top().stack.pop();
                    if it.is_generator() {
                        // Drive one `.next()` via the call machinery; the
                        // iterator-result lands on this frame's stack when the
                        // generator yields/returns (or synchronously if done).
                        let next_fn = get_property(&it, &Value::string("next"));
                        self.invoke(module, next_fn, Vec::new(), it, false);
                    } else {
                        // Array-iterator object: step by index synchronously.
                        let result = step_array_iterator(&it);
                        self.top().stack.push(result);
                    }
                }
                Opcode::ObjectKeys => {
                    let v = self.top().stack.pop();
                    let keys: Vec<String> = match v.data() {
                        ValueData::Object(o) => {
                            let b = o.borrow();
                            // For arrays, expose numeric indices (skip "length").
                            if b.is_exotic_array {
                                (0..b.properties.len().saturating_sub(1))
                                    .map(|i| i.to_string())
                                    .collect()
                            } else {
                                b.properties.keys().cloned().collect()
                            }
                        }
                        ValueData::String(s) => (0..s.chars().count()).map(|i| i.to_string()).collect(),
                        _ => Vec::new(),
                    };
                    let o = js_runtime::object::ObjectData::new_handle();
                    {
                        let mut obj = o.borrow_mut();
                        obj.class = "Array";
                        obj.is_exotic_array = true;
                        for (i, k) in keys.iter().enumerate() {
                            obj.properties.insert(
                                i.to_string(),
                                js_runtime::object::PropertyDescriptor::data(Value::string(k.clone())),
                            );
                        }
                        obj.properties.insert(
                            "length".to_string(),
                            js_runtime::object::PropertyDescriptor::data(Value::integer(keys.len() as i32)),
                        );
                    }
                    self.top().stack.push(Value::object(o));
                }

                // ---- stack / constants ----
                Opcode::LdaUndefined => self.top().stack.push(Value::undefined()),
                Opcode::LdaNull => self.top().stack.push(Value::null()),
                Opcode::LdaTrue => self.top().stack.push(Value::boolean(true)),
                Opcode::LdaFalse => self.top().stack.push(Value::boolean(false)),
                Opcode::LdaConst => {
                    let v = module.constants.get(ins.operand).clone();
                    self.top().stack.push(v);
                }
                Opcode::LdaFunction => {
                    let f = self.function_value(module, ins.operand as u32);
                    self.top().stack.push(Value::function(f));
                }
                Opcode::LdaThis => {
                    let v = self.current_this();
                    self.top().stack.push(v);
                }
                Opcode::LdaUpvalue => {
                    let v = self.top().upvalues[ins.operand as usize].borrow().clone();
                    self.top().stack.push(v);
                }
                Opcode::StaUpvalue => {
                    let v = self.top().stack.pop();
                    *self.top().upvalues[ins.operand as usize].borrow_mut() = v;
                }
                Opcode::LdaLocal => {
                    let v = self.top().locals[ins.operand as usize].borrow().clone();
                    self.top().stack.push(v);
                }
                Opcode::StaLocal => {
                    let v = self.top().stack.pop();
                    *self.top().locals[ins.operand as usize].borrow_mut() = v;
                }
                Opcode::Pop => {
                    self.top().stack.pop();
                }
                Opcode::Dup => {
                    let v = self.top().stack.peek().clone();
                    self.top().stack.push(v);
                }
                Opcode::Swap => {
                    let b = self.top().stack.pop();
                    let a = self.top().stack.pop();
                    self.top().stack.push(b);
                    self.top().stack.push(a);
                }

                // ---- arithmetic / binary ----
                Opcode::Add => self.binop(add),
                Opcode::Sub => self.binop(sub),
                Opcode::Mul => self.binop(mul),
                Opcode::Div => self.binop(div),
                Opcode::Mod => self.binop(rem),
                Opcode::Exp => self.binop(pow),
                Opcode::BitAnd => self.binop(bitand),
                Opcode::BitOr => self.binop(bitor),
                Opcode::BitXor => self.binop(bitxor),
                Opcode::Shl => self.binop(shl),
                Opcode::Shr => self.binop(shr),

                // ---- comparison ----
                Opcode::Eq => self.cmp(eq_loose),
                Opcode::StrictEq => self.cmp(eq_strict),
                Opcode::Lt => self.cmp(cmp_lt),
                Opcode::Le => self.cmp(cmp_le),
                Opcode::Gt => self.cmp(cmp_gt),
                Opcode::Ge => self.cmp(cmp_ge),
                Opcode::Instanceof => {
                    let b = self.top().stack.pop();
                    let a = self.top().stack.pop();
                    self.top().stack.push(Value::boolean(crate::builtins::instanceof_check(&a, &b)));
                }

                // ---- unary ----
                Opcode::Neg => self.unary(neg),
                Opcode::Pos => self.unary(pos),
                Opcode::Not => {
                    let b = is_falsy(&self.top().stack.pop());
                    self.top().stack.push(Value::boolean(b));
                }
                Opcode::BitNot => self.unary(bitnot),
                Opcode::Typeof => self.unary(typeof_),

                // ---- globals ----
                Opcode::GetGlobal => {
                    let name = match module.constants.get(ins.operand).data() {
                        ValueData::String(s) => s.as_str().to_string(),
                        _ => String::new(),
                    };
                    let v = self
                        .ctx
                        .realm
                        .borrow()
                        .globals
                        .get(&name)
                        .cloned()
                        .unwrap_or_default();
                    self.top().stack.push(v);
                }
                Opcode::SetGlobal => {
                    let v = self.top().stack.pop();
                    let name = match module.constants.get(ins.operand).data() {
                        ValueData::String(s) => s.as_str().to_string(),
                        _ => String::new(),
                    };
                    self.ctx.realm.borrow_mut().globals.insert(name, v);
                }

                // ---- control flow ----
                Opcode::Jump => {
                    self.top().pc = ins.operand as usize;
                }
                Opcode::JumpIfTrue => {
                    let v = self.top().stack.pop();
                    if is_truthy(&v) {
                        self.top().pc = ins.operand as usize;
                    }
                }
                Opcode::JumpIfFalse => {
                    let v = self.top().stack.pop();
                    if is_falsy(&v) {
                        self.top().pc = ins.operand as usize;
                    }
                }
                Opcode::Return => {
                    let (ret, was_construct, this_obj, gen) = {
                        let f = self.frames.last_mut().unwrap();
                        (f.stack.pop(), f.is_construct, f.this.clone(), f.generator.clone())
                    };
                    // A generator body completing: mark done, return {value, done:true}.
                    if let Some(g) = gen {
                        g.borrow_mut().done = true;
                        self.frames.pop();
                        if self.frames.is_empty() {
                            return Ok(Step::Done(Value::undefined()));
                        }
                        self.top().stack.push(iter_result(ret, true));
                        return Ok(Step::More);
                    }
                    self.frames.pop();
                    let ret = if was_construct && !ret.is_object() { this_obj } else { ret };
                    if self.frames.is_empty() {
                        return Ok(Step::Done(ret));
                    }
                    self.top().stack.push(ret);
                }
                Opcode::Throw => {
                    let v = self.top().stack.pop();
                    return Err(InterpError::Throw(v));
                }
                Opcode::TryBegin => {
                    let idx = self.top().func_index;
                    let spec = func_ref(module, idx).handlers[ins.operand as usize];
                    self.top().try_stack.push(crate::frame::ActiveTry {
                        catch_pc: spec.catch_pc,
                        finally_pc: spec.finally_pc,
                    });
                }
                Opcode::TryEnd => {
                    self.top().try_stack.pop();
                }
                Opcode::FinallyEnd => {
                    // Re-raise the pending exception (try/finally with no catch),
                    // else continue normally.
                    if let Some(v) = self.top().pending_throw.take() {
                        return Err(InterpError::Throw(v));
                    }
                }
                Opcode::Yield => {
                    // Suspend the current generator frame: pop the yielded
                    // value, save the frame back into its generator, then pop
                    // the frame and hand {value, done:false} to the caller.
                    let gen = self.frames.last().unwrap().generator.clone();
                    let gen = gen.expect("`yield` outside a generator frame");
                    let yielded = self.frames.last_mut().unwrap().stack.pop();
                    {
                        let mut s = gen.borrow_mut();
                        let f = self.frames.last().unwrap();
                        s.pc = f.pc;
                        s.locals = self.frames.last_mut().unwrap().locals.split_off(0);
                        let depth = self.frames.last().unwrap().stack.depth();
                        let mut v: Vec<Value> = (0..depth)
                            .map(|_| self.frames.last_mut().unwrap().stack.pop())
                            .collect();
                        v.reverse();
                        s.stack = v;
                        s.upvalues = self.frames.last_mut().unwrap().upvalues.split_off(0);
                        s.this = self.frames.last_mut().unwrap().this.clone();
                        s.captured_this = self.frames.last_mut().unwrap().captured_this.take();
                        s.done = false;
                    }
                    self.frames.pop();
                    if self.frames.is_empty() {
                        // `yield` at the top of the script (no caller) — drop it.
                        return Ok(Step::Done(Value::undefined()));
                    }
                    self.top().stack.push(iter_result(yielded, false));
                }

                // ---- calls ----
                Opcode::Call => {
                    let (callee, args) = pop_args(self, ins.operand);
                    self.invoke(module, callee, args, Value::undefined(), false);
                }
                Opcode::CallMethod => {
                    // Stack: [obj, fn, args...]
                    let (args, this, callee) = {
                        let n = ins.operand as usize;
                        let frame = self.frames.last_mut().unwrap();
                        let mut a: Vec<Value> = (0..n).map(|_| frame.stack.pop()).collect();
                        a.reverse();
                        let callee = frame.stack.pop();
                        let this = frame.stack.pop();
                        (a, this, callee)
                    };
                    self.invoke(module, callee, args, this, false);
                }
                Opcode::NewObject => {
                    let o = js_runtime::object::ObjectData::new_handle();
                    self.top().stack.push(Value::object(o));
                }
                Opcode::NewRegex => {
                    // Constant holds "pattern\0flags".
                    let combined = match module.constants.get(ins.operand).data() {
                        ValueData::String(s) => s.as_str().to_string(),
                        _ => String::new(),
                    };
                    let (pattern, flags) = combined.split_once('\0')
                        .map(|(p, f)| (p.to_string(), f.to_string()))
                        .unwrap_or((combined.clone(), String::new()));
                    let global = flags.contains('g');
                    let ignore_case = flags.contains('i');
                    let multiline = flags.contains('m');
                    let dotall = flags.contains('s');
                    let sticky = flags.contains('y');
                    let unicode = flags.contains('u');
                    let o = js_runtime::object::ObjectData::new_handle();
                    {
                        let mut b = o.borrow_mut();
                        b.class = "RegExp";
                        let pd = |v: Value| js_runtime::object::PropertyDescriptor::data(v);
                        b.properties.insert("source".into(), pd(Value::string(pattern)));
                        b.properties.insert("flags".into(), pd(Value::string(flags)));
                        b.properties.insert("global".into(), pd(Value::boolean(global)));
                        b.properties.insert("ignoreCase".into(), pd(Value::boolean(ignore_case)));
                        b.properties.insert("multiline".into(), pd(Value::boolean(multiline)));
                        b.properties.insert("dotAll".into(), pd(Value::boolean(dotall)));
                        b.properties.insert("sticky".into(), pd(Value::boolean(sticky)));
                        b.properties.insert("unicode".into(), pd(Value::boolean(unicode)));
                        b.properties.insert("lastIndex".into(), pd(Value::integer(0)));
                    }
                    self.top().stack.push(Value::object(o));
                }
                Opcode::NewArray => {
                    let n = ins.operand as usize;
                    let mut args: Vec<Value> = (0..n).map(|_| self.top().stack.pop()).collect();
                    args.reverse();
                    let o = js_runtime::object::ObjectData::new_handle();
                    {
                        let mut obj = o.borrow_mut();
                        obj.class = "Array";
                        obj.is_exotic_array = true;
                        for (i, v) in args.iter().enumerate() {
                            obj.properties.insert(
                                i.to_string(),
                                js_runtime::object::PropertyDescriptor::data(v.clone()),
                            );
                        }
                        obj.properties.insert(
                            "length".to_string(),
                            js_runtime::object::PropertyDescriptor::data(Value::integer(n as i32)),
                        );
                    }
                    self.top().stack.push(Value::object(o));
                }
                Opcode::GetProp => {
                    let key = self.top().stack.pop();
                    let obj = self.top().stack.pop();
                    self.top().stack.push(get_property(&obj, &key));
                }
                Opcode::SetProp => {
                    // Stack: [..., value, obj, key] (key on top). Pops key, obj,
                    // value; the compiler keeps the assignment result via a
                    // preceding `Dup` of the value.
                    let key = self.top().stack.pop();
                    let obj = self.top().stack.pop();
                    let value = self.top().stack.pop();
                    set_property(&obj, &key, value);
                }
                Opcode::New => {
                    let (callee, args) = pop_args(self, ins.operand);
                    // Construct a fresh object bound as `this`.
                    let this = Value::object(js_runtime::object::ObjectData::new_handle());
                    self.invoke(module, callee, args, this, true);
                }

                Opcode::LogicalAnd | Opcode::LogicalOr | Opcode::NullishCoal => {
                    return Err(InterpError::Internal(format!(
                        "opcode {:?} not implemented yet (lowered to runtime calls)",
                        ins.op
                    )));
                }
            }
        Ok(Step::More)
    }

    /// Short-hand for the currently executing frame.
    fn top(&mut self) -> &mut CallFrame {
        self.frames.last_mut().unwrap()
    }

    /// Call a JS `callee` with `args`/`this` and run it to completion
    /// synchronously (used by native builtins that take callbacks, e.g.
    /// `Array.map`). Runs a sub-dispatch loop until the callee's frame returns,
    /// then pops and returns its return value.
    pub(crate) fn call_value(
        &mut self,
        module: &BytecodeModule,
        callee: Value,
        args: Vec<Value>,
        this: Value,
    ) -> Result<Value, InterpError> {
        let target = self.frames.len();
        self.invoke(module, callee, args, this, false);
        // `invoke` may have pushed a frame (bytecode callee) or already pushed
        // a result (native/done-generator). Step until the frame unwinds.
        while self.frames.len() > target {
            match self.step(module) {
                Ok(Step::More) => {}
                Ok(Step::Done(_)) => break,
                Err(e) => match self.handle_exception(e, target) {
                    Ok(()) => {}                  // caught inside the callee
                    Err(e) => return Err(e),      // escaped the callee → propagate
                },
            }
        }
        // The callee's return value now sits on top of the caller's stack.
        Ok(self.top().stack.pop())
    }

    /// Pop `b`, then `a`, apply `f`, push the result.
    fn binop<F: Fn(Value, Value) -> Value>(&mut self, f: F) {
        let b = self.top().stack.pop();
        let a = self.top().stack.pop();
        self.top().stack.push(f(a, b));
    }

    fn cmp<F: Fn(Value, Value) -> bool>(&mut self, f: F) {
        let b = self.top().stack.pop();
        let a = self.top().stack.pop();
        self.top().stack.push(Value::boolean(f(a, b)));
    }

    fn unary<F: Fn(Value) -> Value>(&mut self, f: F) {
        let a = self.top().stack.pop();
        self.top().stack.push(f(a));
    }

    /// Build a [`JsFunction`] value for function-table index `id`, capturing
    /// upvalue cells from the currently executing frame per the function's
    /// compiled [`UpvalueSpec`]s.
    fn function_value(&mut self, module: &BytecodeModule, id: u32) -> JsFunction {
        let func = func_ref(module, id as usize);
        let is_arrow = func.is_arrow;
        // Capture upvalue cells from the current (enclosing) frame.
        let mut upvalues = Vec::with_capacity(func.upvalues.len());
        for spec in &func.upvalues {
            let frame = self.frames.last().unwrap();
            let cell = if spec.is_local {
                frame.locals[spec.index as usize].clone()
            } else {
                frame.upvalues[spec.index as usize].clone()
            };
            upvalues.push(cell);
        }
        // Arrows capture `this` lexically from the enclosing frame.
        let this_cell = if is_arrow {
            Some(new_cell(self.current_this()))
        } else {
            None
        };
        let mut f = JsFunction::new(func.name.clone(), id, func.param_count);
        f.upvalues = upvalues;
        f.this_cell = this_cell;
        f
    }

    /// The `this` value for the current frame (ordinary frame `this`, or the
    /// arrow's lexically captured `this`).
    fn current_this(&self) -> Value {
        let frame = self.frames.last().unwrap();
        if let Some(c) = &frame.captured_this {
            return c.borrow().clone();
        }
        frame.this.clone()
    }

    /// Push a new frame for `callee` with the given `this` binding and args.
    /// `is_construct` marks a `new` invocation, which keeps the fresh object as
    /// the result unless the constructor explicitly returns an object.
    fn invoke(
        &mut self,
        module: &BytecodeModule,
        callee: Value,
        args: Vec<Value>,
        this: Value,
        is_construct: bool,
    ) {
        let f = match callee.as_function() {
            Some(f) => f.clone(),
            None => {
                self.top().stack.push(if is_construct { this } else { Value::undefined() });
                return;
            }
        };
        // Native (builtin) function: run it and handle the result.
        if let Some(nid) = f.native {
            // SAFETY: a native call never mutates `self.natives` (the table is
            // fixed after construction), so a `*const` borrow is stable for the
            // call's duration even though `self` is borrowed mutably inside it.
            let nf_ptr: *const dyn NativeFn = self.natives[nid as usize].as_ref();
            let result = unsafe { (&*nf_ptr).call(self, module, this, &f, args) };
            match result {
                Ok(NativeResult::Value(v)) => self.top().stack.push(v),
                Ok(NativeResult::ResumeGenerator(gen, arg)) => {
                    self.checkout_generator(gen, arg);
                }
                Err(e) => self.pending_err = Some(e),
            }
            return;
        }
        let id = f.id as usize;
        let func = func_ref(module, id);
        // Calling a `function*` creates a generator object (it does not run).
        if func.is_generator && !is_construct {
            let state = self.make_generator_state(module, id, &f, args, this);
            self.top().stack.push(Value::generator(Rc::new(RefCell::new(state))));
            return;
        }
        let (slot_count, param_count, span) = func_meta(module, id);
        let mut nf = CallFrame::new(id, slot_count, span);
        for i in 0..(param_count as usize).min(args.len()) {
            nf.locals[i] = new_cell(args[i].clone());
        }
        nf.upvalues = f.upvalues.clone();
        if f.this_cell.is_some() {
            nf.captured_this = f.this_cell.clone();
        } else {
            nf.this = this;
        }
        nf.is_construct = is_construct;
        self.frames.push(nf);
    }

    /// Build the suspended state for a freshly-called generator function.
    fn make_generator_state(
        &self,
        module: &BytecodeModule,
        id: usize,
        f: &JsFunction,
        args: Vec<Value>,
        this: Value,
    ) -> GeneratorState {
        let func = func_ref(module, id);
        let slot_count = func.locals.slot_count();
        let mut locals: Vec<_> = (0..slot_count).map(|_| new_cell(Value::undefined())).collect();
        for i in 0..(func.param_count as usize).min(args.len()) {
            locals[i] = new_cell(args[i].clone());
        }
        GeneratorState {
            func_index: id as u32,
            pc: 0,
            locals,
            stack: Vec::new(),
            upvalues: f.upvalues.clone(),
            this: if f.this_cell.is_some() { Value::undefined() } else { this },
            captured_this: f.this_cell.clone(),
            done: false,
            started: false,
        }
    }

    /// Resume a paused generator: check its frame state out into a live
    /// `CallFrame`, push the `.next(arg)` argument (for non-first resumes), and
    /// push the frame so the dispatch loop continues it.
    fn checkout_generator(&mut self, gen: Rc<RefCell<GeneratorState>>, arg: Value) {
        let (done, started, func_index, pc, locals, stack, upvalues, this, captured_this) = {
            let mut s = gen.borrow_mut();
            if s.done {
                // Already finished: return {undefined, true}.
                drop(s);
                self.top().stack.push(iter_result(Value::undefined(), true));
                return;
            }
            let started = s.started;
            s.started = true;
            (
                false,
                started,
                s.func_index,
                s.pc,
                std::mem::take(&mut s.locals),
                std::mem::take(&mut s.stack),
                std::mem::take(&mut s.upvalues),
                std::mem::replace(&mut s.this, Value::undefined()),
                s.captured_this.take(),
            )
        };
        let _ = done;
        let mut frame = CallFrame::new(func_index as usize, 0, js_syntax::Span::DUMMY);
        frame.pc = pc;
        frame.locals = locals;
        frame.upvalues = upvalues;
        frame.this = this;
        frame.captured_this = captured_this;
        frame.generator = Some(gen);
        for v in stack {
            frame.stack.push(v);
        }
        if started {
            // The `.next(arg)` argument is the value of the `yield` expression.
            frame.stack.push(arg);
        }
        self.frames.push(frame);
    }
}

/// Build the default native-function table (generator `.next`/`.return`/`.
/// throw`, then the Array/String builtin methods).
fn default_natives() -> Vec<Box<dyn NativeFn>> {
    let mut v: Vec<Box<dyn NativeFn>> = vec![
        Box::new(GenNext),
        Box::new(GenReturn),
        Box::new(GenThrow),
    ];
    v.extend(crate::builtins::all_builtins());
    v
}

/// Construct an iterator-result object `{ value, done }`.
pub(crate) fn iter_result(value: Value, done: bool) -> Value {
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.properties.insert(
            "value".to_string(),
            js_runtime::object::PropertyDescriptor::data(value),
        );
        b.properties.insert(
            "done".to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::boolean(done)),
        );
    }
    Value::object(o)
}

/// Pop `n` args and the callee from the current frame's operand stack.
fn pop_args(interp: &mut Interpreter, n: u16) -> (Value, Vec<Value>) {
    let frame = interp.frames.last_mut().unwrap();
    let n = n as usize;
    let mut args: Vec<Value> = (0..n).map(|_| frame.stack.pop()).collect();
    args.reverse();
    let callee = frame.stack.pop();
    (callee, args)
}

fn func_ref<'a>(module: &'a BytecodeModule, index: usize) -> &'a BytecodeFunction {
    if index == 0 {
        &module.main
    } else {
        &module.functions[index - 1]
    }
}

/// `(slot_count, param_count, span)` for a function by table id.
fn func_meta(module: &BytecodeModule, id: usize) -> (u16, u16, js_syntax::Span) {
    let f = func_ref(module, id);
    (f.locals.slot_count(), f.param_count, f.span)
}

// ---- value semantics (milestone subset) ----------------------------------

fn as_f64(v: &ValueData) -> Option<f64> {
    match v {
        ValueData::Number(n) => Some(*n),
        ValueData::Integer(i) => Some(*i as f64),
        ValueData::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
        ValueData::Null => Some(0.0),
        ValueData::Undefined => Some(f64::NAN),
        _ => None,
    }
}

fn num_value(v: Value) -> Value {
    match v.data().clone() {
        ValueData::Integer(i) => Value::integer(i),
        ValueData::Number(n) => Value::number(n),
        other => match as_f64(&other) {
            Some(n) if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 => {
                Value::integer(n as i32)
            }
            Some(n) => Value::number(n),
            None => Value::number(f64::NAN),
        },
    }
}

fn add(a: Value, b: Value) -> Value {
    use ValueData::*;
    match (a.data().clone(), b.data().clone()) {
        // String concatenation: if either side is a string, coerce both to string.
        (String(_), _) | (_, String(_)) => Value::string(to_string(&a) + &to_string(&b)),
        (Integer(x), Integer(y)) => match x.checked_add(y) {
            Some(z) => Value::integer(z),
            None => Value::number(x as f64 + y as f64),
        },
        _ => Value::number(num_f64(&a) + num_f64(&b)),
    }
}

fn sub(a: Value, b: Value) -> Value {
    if let (Some(x), Some(y)) = (as_int(&a), as_int(&b)) {
        if let Some(z) = x.checked_sub(y) {
            return Value::integer(z);
        }
    }
    Value::number(num_f64(&a) - num_f64(&b))
}

fn mul(a: Value, b: Value) -> Value {
    if let (Some(x), Some(y)) = (as_int(&a), as_int(&b)) {
        if let Some(z) = x.checked_mul(y) {
            return Value::integer(z);
        }
    }
    Value::number(num_f64(&a) * num_f64(&b))
}

fn div(a: Value, b: Value) -> Value {
    Value::number(num_f64(&a) / num_f64(&b))
}

fn rem(a: Value, b: Value) -> Value {
    Value::number(num_f64(&a) % num_f64(&b))
}

fn pow(a: Value, b: Value) -> Value {
    Value::number(num_f64(&a).powf(num_f64(&b)))
}

fn as_int(v: &Value) -> Option<i32> {
    match v.data() {
        ValueData::Integer(i) => Some(*i),
        ValueData::Number(n) if n.fract() == 0.0 && n.is_finite() => Some(*n as i32),
        _ => None,
    }
}

fn num_f64(v: &Value) -> f64 {
    as_f64(v.data()).unwrap_or(f64::NAN)
}

fn bitand(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a) & to_int32(&b))
}
fn bitor(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a) | to_int32(&b))
}
fn bitxor(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a) ^ to_int32(&b))
}
fn shl(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a).wrapping_shl((to_uint32(&b) & 31) as u32))
}
fn shr(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a).wrapping_shr((to_uint32(&b) & 31) as u32))
}

fn to_int32(v: &Value) -> i32 {
    num_f64(v) as i32
}
fn to_uint32(v: &Value) -> u32 {
    num_f64(v) as u32
}

fn neg(a: Value) -> Value {
    match a.data() {
        ValueData::Integer(i) => Value::integer(i.wrapping_neg()),
        _ => Value::number(-num_f64(&a)),
    }
}
fn pos(a: Value) -> Value {
    num_value(a)
}
fn bitnot(a: Value) -> Value {
    Value::integer(!to_int32(&a))
}
fn typeof_(a: Value) -> Value {
    let s = match a.data() {
        ValueData::Undefined => "undefined",
        ValueData::Null => "object",
        ValueData::Boolean(_) => "boolean",
        ValueData::Number(_) | ValueData::Integer(_) => "number",
        ValueData::String(_) => "string",
        ValueData::Function(_) => "function",
        ValueData::Symbol(_) => "symbol",
        ValueData::BigInt(_) => "bigint",
        ValueData::Object(_) => "object",
        ValueData::Generator(_) => "object",
    };
    Value::string(s)
}

fn eq_strict(a: Value, b: Value) -> bool {
    use ValueData::*;
    match (a.data(), b.data()) {
        (Integer(x), Integer(y)) => x == y,
        (Integer(x), Number(y)) => (*x as f64) == *y,
        (Number(x), Integer(y)) => *x == (*y as f64),
        (Number(x), Number(y)) => x == y,
        (String(x), String(y)) => x == y,
        (Boolean(x), Boolean(y)) => x == y,
        (Null, Null) | (Undefined, Undefined) => true,
        (Object(x), Object(y)) => Rc::ptr_eq(x, y),
        (Function(x), Function(y)) => x.id == y.id,
        (Symbol(_), _) | (_, Symbol(_)) => false, // symbols compare by identity (TODO)
        _ => false,
    }
}

fn eq_loose(a: Value, b: Value) -> bool {
    use ValueData::*;
    match (a.data(), b.data()) {
        (Undefined, Null) | (Null, Undefined) => true,
        // Same type → fall back to strict equality.
        _ if std::mem::discriminant(a.data()) == std::mem::discriminant(b.data()) => {
            eq_strict(a, b)
        }
        // Otherwise: loose numeric coercion for the milestone subset.
        _ => {
            let na = as_f64(a.data());
            let nb = as_f64(b.data());
            match (na, nb) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            }
        }
    }
}

fn cmp_lt(a: Value, b: Value) -> bool {
    num_f64(&a) < num_f64(&b)
}
fn cmp_le(a: Value, b: Value) -> bool {
    num_f64(&a) <= num_f64(&b)
}
fn cmp_gt(a: Value, b: Value) -> bool {
    num_f64(&a) > num_f64(&b)
}
fn cmp_ge(a: Value, b: Value) -> bool {
    num_f64(&a) >= num_f64(&b)
}

fn is_truthy(v: &Value) -> bool {
    !is_falsy(v)
}

fn is_falsy(v: &Value) -> bool {
    match v.data() {
        ValueData::Undefined | ValueData::Null => true,
        ValueData::Boolean(b) => !b,
        ValueData::Integer(i) => *i == 0,
        ValueData::Number(n) => *n == 0.0 || n.is_nan(),
        ValueData::String(s) => s.is_empty(),
        _ => false,
    }
}

pub(crate) fn to_string(v: &Value) -> String {
    match v.data() {
        ValueData::Undefined => "undefined".to_string(),
        ValueData::Null => "null".to_string(),
        ValueData::Boolean(b) => b.to_string(),
        ValueData::Integer(i) => i.to_string(),
        ValueData::Number(n) => format_number(*n),
        ValueData::String(s) => s.as_str().to_string(),
        ValueData::Object(o) => {
            let b = o.borrow();
            if b.is_exotic_array {
                // Array.prototype.toString === join(",") of elements.
                let len = b
                    .properties
                    .get("length")
                    .and_then(|d| match d {
                        js_runtime::object::PropertyDescriptor::Data { value, .. } => match value.data() {
                            ValueData::Integer(i) => Some(*i as usize),
                            ValueData::Number(n) => Some(*n as usize),
                            _ => None,
                        },
                        _ => None,
                    })
                    .unwrap_or(0);
                let parts: Vec<String> = (0..len)
                    .map(|i| {
                        let e = b
                            .properties
                            .get(&i.to_string())
                            .and_then(|d| match d {
                                js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                                _ => None,
                            })
                            .unwrap_or_else(Value::undefined);
                        if e.is_null() || e.is_undefined() {
                            String::new()
                        } else {
                            to_string(&e)
                        }
                    })
                    .collect();
                drop(b);
                parts.join(",")
            } else {
                // Error-like objects: if they have `name` + `message`,
                // display as "name: message" for readable output.
                let (name_v, msg_v) = {
                    let nv = b.properties.get("name")
                        .and_then(|d| match d {
                            js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                            _ => None,
                        });
                    let mv = b.properties.get("message")
                        .and_then(|d| match d {
                            js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                            _ => None,
                        });
                    (nv, mv)
                };
                drop(b);
                match (name_v, msg_v) {
                    (Some(nv), Some(mv)) => {
                        let n = to_string(&nv);
                        let m = to_string(&mv);
                        if n.ends_with("Error") {
                            if m.is_empty() { n } else { format!("{}: {}", n, m) }
                        } else {
                            "[object Object]".to_string()
                        }
                    }
                    _ => "[object Object]".to_string(),
                }
            }
        }
        _ => "[object Object]".to_string(),
    }
}

fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Convert a property-key value to its canonical string name.
pub(crate) fn prop_name(key: &Value) -> String {
    match key.data() {
        ValueData::String(s) => s.as_str().to_string(),
        ValueData::Integer(i) => i.to_string(),
        ValueData::Number(n) => format_number(*n),
        ValueData::Boolean(b) => b.to_string(),
        ValueData::Null => "null".to_string(),
        ValueData::Undefined => "undefined".to_string(),
        _ => to_string(key),
    }
}

/// `obj[key]` / `obj.key`. Walks the prototype chain; returns `undefined` for
/// missing properties (no throw in the milestone subset). Strings expose
/// `.length` and integer indexing.
pub(crate) fn get_property(obj: &Value, key: &Value) -> Value {
    let name = prop_name(key);
    // Static methods on global constructor functions (Number.isInteger etc.).
    if let Some(nid) = crate::builtins::native_static_id(obj, &name) {
        let mut f = JsFunction::new(name.clone(), 0, 1);
        f.native = Some(nid);
        return Value::function(f);
    }
    // Known builtin instance methods on arrays/strings resolve to native fns.
    if let Some(bid) = crate::builtins::builtin_method_id(obj, &name) {
        let mut f = JsFunction::new(name.clone(), 0, 1);
        f.native = Some(bid);
        return Value::function(f);
    }
    // Generator objects expose `.next` / `.return` / `.throw` as bound native
    // methods.
    if let ValueData::Generator(g) = obj.data() {
        let native = match name.as_str() {
            "next" => Some(native_id::GEN_NEXT),
            "return" => Some(native_id::GEN_RETURN),
            "throw" => Some(native_id::GEN_THROW),
            _ => None,
        };
        if let Some(nid) = native {
            let mut f = JsFunction::new(name.clone(), 0, 1);
            f.native = Some(nid);
            f.bound_generator = Some(g.clone());
            return Value::function(f);
        }
        return Value::undefined();
    }
    // Primitive string: `.length` and index access.
    if let ValueData::String(s) = obj.data() {
        if name == "length" {
            return Value::integer(s.chars().count() as i32);
        }
        if let Ok(i) = name.parse::<usize>() {
            if let Some(c) = s.as_str().chars().nth(i) {
                return Value::string(c.to_string());
            }
        }
        return Value::undefined();
    }
    // Objects (incl. arrays): dictionary lookup + proto chain.
    let Some(handle) = obj_as_object(obj) else {
        return Value::undefined();
    };
    let mut cur = Some(handle.clone());
    while let Some(h) = cur {
        let b = h.borrow();
        if let Some(desc) = b.properties.get(&name) {
            return match desc {
                js_runtime::object::PropertyDescriptor::Data { value, .. } => value.clone(),
                js_runtime::object::PropertyDescriptor::Accessor { get, .. } => {
                    // Accessors aren't invoked in the milestone subset.
                    get.clone().unwrap_or_else(Value::undefined)
                }
            };
        }
        cur = b.proto.as_ref().and_then(|v| obj_as_object(v)).cloned();
    }
    Value::undefined()
}

/// `obj[key] = value` for object/array targets.
pub(crate) fn set_property(obj: &Value, key: &Value, value: Value) {
    let Some(handle) = obj_as_object(obj) else {
        return; // silently ignore writes to primitives.
    };
    let name = prop_name(key);
    let is_array = handle.borrow().is_exotic_array;
    let new_len = if is_array {
        name.parse::<usize>().ok()
    } else {
        None
    };
    let mut b = handle.borrow_mut();
    b.properties
        .insert(name, js_runtime::object::PropertyDescriptor::data(value));
    if let Some(idx) = new_len {
        let cur = b
            .properties
            .get("length")
            .and_then(|d| match d {
                js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            .and_then(|v| match v.data() {
                ValueData::Integer(i) => Some(*i as usize),
                ValueData::Number(n) => Some(*n as usize),
                _ => None,
            })
            .unwrap_or(0);
        if idx >= cur {
            b.properties.insert(
                "length".to_string(),
                js_runtime::object::PropertyDescriptor::data(Value::integer((idx + 1) as i32)),
            );
        }
    }
}

/// Build an iterator value for an iterable. Generators are their own iterator;
/// arrays and strings get an index-cursor object.
fn make_iterator(iterable: &Value) -> Value {
    if iterable.is_generator() {
        return iterable.clone();
    }
    let is_indexable = matches!(
        iterable.data(),
        ValueData::Object(o) if o.borrow().is_exotic_array)
        || matches!(iterable.data(), ValueData::String(_));
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.class = "ArrayIterator";
        b.properties.insert(
            IT_SRC.to_string(),
            js_runtime::object::PropertyDescriptor::data(if is_indexable {
                iterable.clone()
            } else {
                Value::undefined()
            }),
        );
        b.properties.insert(
            IT_IDX.to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::integer(0)),
        );
    }
    Value::object(o)
}

/// Step an array-iterator object once: read `__it_src`/`__it_idx`, produce the
/// next `{value, done}` result, and bump the index.
fn step_array_iterator(it: &Value) -> Value {
    let src = get_property(it, &Value::string(IT_SRC));
    let idx = get_property(it, &Value::string(IT_IDX));
    let len = get_property(&src, &Value::string("length"));
    let i = match idx.data() {
        ValueData::Integer(i) => *i,
        ValueData::Number(n) => *n as i32,
        _ => -1,
    };
    let l = match len.data() {
        ValueData::Integer(i) => *i,
        ValueData::Number(n) => *n as i32,
        _ => 0,
    };
    if i < l {
        let value = get_property(&src, &Value::string(i.to_string()));
        set_property(
            it,
            &Value::string(IT_IDX),
            Value::integer(i + 1),
        );
        iter_result(value, false)
    } else {
        iter_result(Value::undefined(), true)
    }
}

/// Append `v` to an array-object (sets the next numeric index, bumps `length`).
/// Construct an array-object from a vec of values (sets numeric indices + length).
pub(crate) fn make_array(vals: Vec<Value>) -> Value {
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.class = "Array";
        b.is_exotic_array = true;
        for (i, v) in vals.iter().enumerate() {
            b.properties.insert(
                i.to_string(),
                js_runtime::object::PropertyDescriptor::data(v.clone()),
            );
        }
        b.properties.insert(
            "length".to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::integer(vals.len() as i32)),
        );
    }
    Value::object(o)
}

/// Read an array/object's numeric length (0 for non-arrays).
pub(crate) fn array_len(arr: &Value) -> usize {
    match arr.data() {
        ValueData::Object(o) => {
            let b = o.borrow();
            b.properties
                .get("length")
                .and_then(|d| match d {
                    js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                    _ => None,
                })
                .and_then(|v| match v.data() {
                    ValueData::Integer(i) => Some(*i as usize),
                    ValueData::Number(n) => Some(*n as usize),
                    _ => None,
                })
                .unwrap_or(0)
        }
        ValueData::String(s) => s.chars().count(),
        _ => 0,
    }
}

/// Read element `i` of an array (undefined if missing).
pub(crate) fn array_get(arr: &Value, i: usize) -> Value {
    get_property(arr, &Value::string(i.to_string()))
}

pub(crate) fn array_append(arr: &Value, v: Value) {
    let Some(handle) = obj_as_object(arr) else { return; };
    let mut b = handle.borrow_mut();
    let len = b
        .properties
        .get("length")
        .and_then(|d| match d {
            js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            _ => None,
        })
        .and_then(|v| match v.data() {
            ValueData::Integer(i) => Some(*i as usize),
            ValueData::Number(n) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(0);
    b.properties
        .insert(len.to_string(), js_runtime::object::PropertyDescriptor::data(v));
    b.properties.insert(
        "length".to_string(),
        js_runtime::object::PropertyDescriptor::data(Value::integer((len + 1) as i32)),
    );
}

/// Borrow the `JsObject` handle from an object Value, if it is one.
fn obj_as_object(v: &Value) -> Option<&js_runtime::object::JsObject> {
    match v.data() {
        ValueData::Object(o) => Some(o),
        _ => None,
    }
}

/// Run a module end-to-end, lifting [`InterpError`] into a `DiagResult` error.
pub fn run(module: &BytecodeModule, ctx: RealmContext) -> DiagResult<Value> {
    match Interpreter::new(ctx).run_module(module) {
        Ok(v) => Ok(v),
        Err(InterpError::Internal(msg)) => Err(vec![js_diagnostics::Diagnostic::error(
            js_syntax::Span::DUMMY,
            msg,
        )]),
        Err(InterpError::Throw(v)) => Err(vec![js_diagnostics::Diagnostic::error(
            js_syntax::Span::DUMMY,
            format!("Uncaught {:?}", v),
        )]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_undefined_for_empty_module() {
        let module = js_bytecode::compile_program(&js_syntax::ast::Program::new(
            js_syntax::Span::DUMMY,
            js_syntax::ast::ProgramKind::Script,
            vec![],
        ))
        .expect("compile");
        let v = Interpreter::fresh().run_module(&module).expect("run");
        assert!(v.is_undefined());
    }
}
