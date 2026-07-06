//! The dispatch loop.
//!
//! [`Interpreter::run_module`] pushes a frame for `<main>` and dispatches
//! instructions until a `Return` unwinds to the top. Function calls push new
//! frames; `Return` pops them and pushes the return value onto the caller's
//! operand stack — so the whole call tree runs inside one flat loop, no Rust
//! recursion.

use crate::frame::CallFrame;
use js_bytecode::{BytecodeFunction, BytecodeModule, Opcode};
use js_diagnostics::DiagResult;
use js_runtime::context::RealmContext;
use js_runtime::value::{JsFunction, Value, ValueData};
use std::fmt;
use std::rc::Rc;

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
            InterpError::Throw(v) => write!(f, "Uncaught {:?}", v),
            InterpError::Internal(msg) => write!(f, "internal interpreter error: {msg}"),
        }
    }
}

impl std::error::Error for InterpError {}

/// The bytecode interpreter.
pub struct Interpreter {
    ctx: RealmContext,
    frames: Vec<CallFrame>,
}

impl Interpreter {
    pub fn new(ctx: RealmContext) -> Interpreter {
        Interpreter {
            ctx,
            frames: Vec::new(),
        }
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

    fn dispatch(&mut self, module: &BytecodeModule) -> Result<Value, InterpError> {
        loop {
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
                        return Ok(Value::undefined());
                    }
                }
            };

            match ins.op {
                Opcode::Nop => {}

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
                Opcode::LdaLocal => {
                    let v = self
                        .top()
                        .locals
                        .get(ins.operand as usize)
                        .cloned()
                        .unwrap_or_default();
                    self.top().stack.push(v);
                }
                Opcode::StaLocal => {
                    let v = self.top().stack.pop();
                    if let Some(slot) = self.top().locals.get_mut(ins.operand as usize) {
                        *slot = v;
                    }
                }
                Opcode::Pop => {
                    self.top().stack.pop();
                }
                Opcode::Dup => {
                    let v = self.top().stack.peek().clone();
                    self.top().stack.push(v);
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
                    let ret = self.frames.last_mut().unwrap().stack.pop();
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return Ok(ret);
                    }
                    self.top().stack.push(ret);
                }

                // ---- calls ----
                Opcode::Call => {
                    let (callee, args) = {
                        let frame = self.frames.last_mut().unwrap();
                        let n = ins.operand as usize;
                        let mut args: Vec<Value> = (0..n).map(|_| frame.stack.pop()).collect();
                        args.reverse();
                        let callee = frame.stack.pop();
                        (callee, args)
                    };
                    match callee.as_function() {
                        Some(f) => {
                            let id = f.id as usize;
                            let (slot_count, param_count, span) = func_meta(module, id);
                            let mut nf = CallFrame::new(id, slot_count, span);
                            for i in 0..(param_count as usize).min(args.len()) {
                                nf.locals[i] = args[i].clone();
                            }
                            self.frames.push(nf);
                        }
                        None => {
                            // Calling a non-function yields undefined in the
                            // milestone; a real engine would throw a TypeError.
                            self.top().stack.push(Value::undefined());
                        }
                    }
                }
                Opcode::New => {
                    // `new` is not supported for milestone-1; produce undefined.
                    let n = ins.operand as usize;
                    let frame = self.frames.last_mut().unwrap();
                    for _ in 0..n + 1 {
                        frame.stack.pop();
                    }
                    frame.stack.push(Value::undefined());
                }

                // ---- objects / properties (stubbed) ----
                Opcode::NewObject | Opcode::NewArray | Opcode::GetProp | Opcode::SetProp => {
                    return Err(InterpError::Internal(format!(
                        "opcode {:?} not implemented yet",
                        ins.op
                    )));
                }

                Opcode::LogicalAnd | Opcode::LogicalOr | Opcode::NullishCoal => {
                    return Err(InterpError::Internal(format!(
                        "opcode {:?} not implemented yet (lowered to runtime calls)",
                        ins.op
                    )));
                }
            }
        }
    }

    /// Short-hand for the currently executing frame.
    fn top(&mut self) -> &mut CallFrame {
        self.frames.last_mut().unwrap()
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

    /// Build a [`JsFunction`] value for function-table index `id`.
    fn function_value(&self, module: &BytecodeModule, id: u32) -> JsFunction {
        let func = func_ref(module, id as usize);
        JsFunction::new(func.name.clone(), id, func.param_count)
    }
}

// ---- module function lookup ----------------------------------------------

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

fn to_string(v: &Value) -> String {
    match v.data() {
        ValueData::Undefined => "undefined".to_string(),
        ValueData::Null => "null".to_string(),
        ValueData::Boolean(b) => b.to_string(),
        ValueData::Integer(i) => i.to_string(),
        ValueData::Number(n) => format_number(*n),
        ValueData::String(s) => s.as_str().to_string(),
        _ => "[object]".to_string(),
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
