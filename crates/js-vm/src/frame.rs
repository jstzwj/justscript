//! A single call frame: the function being executed, its program counter,
//! locals and operand stack.
//!
//! Locals are **cells** (`Rc<RefCell<Value>>`) rather than bare values so that
//! closures can capture them by reference: an inner function holds the same
//! cell as its enclosing function, and writes from either side are mutually
//! visible. Operand-stack values remain plain `Value`s.

use crate::stack::OperandStack;
use js_runtime::value::Value;
use js_syntax::Span;
use std::cell::RefCell;
use std::rc::Rc;

/// A shared, mutable local-variable cell.
pub type Cell = Rc<RefCell<Value>>;

pub struct CallFrame {
    /// Index of the executing function within the module's function table
    /// (0 = top-level `<main>`).
    pub func_index: usize,
    pub pc: usize,
    pub locals: Vec<Cell>,
    /// Cells captured from the enclosing environment (closures).
    pub upvalues: Vec<Cell>,
    /// The `this` binding for this frame (ordinary functions / `new`).
    pub this: Value,
    /// For arrow functions: the lexically captured `this` cell. `None` for
    /// ordinary functions, which use [`CallFrame::this`].
    pub captured_this: Option<Cell>,
    pub stack: OperandStack,
    /// Source span of the *currently executing* instruction, for backtraces.
    pub span: Span,
    /// True if this frame is a `new` invocation: on `return`, a non-object
    /// result is replaced by the freshly-constructed `this`.
    pub is_construct: bool,
    /// If this frame runs a generator body, the owning generator object — used
    /// by `Yield` to save the frame back and by `Return` to mark it done.
    pub generator: Option<Rc<RefCell<js_runtime::value::GeneratorState>>>,
    /// Active exception handlers (innermost last), pushed by `TryBegin`.
    pub try_stack: Vec<ActiveTry>,
    /// An exception awaiting re-raise after a `finally` block completes.
    pub pending_throw: Option<Value>,
}

/// A live `try` handler: where to go on a caught exception, and the `finally`
/// to run afterwards (each optional).
#[derive(Clone)]
pub struct ActiveTry {
    pub catch_pc: Option<u16>,
    pub finally_pc: Option<u16>,
}

impl CallFrame {
    pub fn new(func_index: usize, slot_count: u16, span: Span) -> CallFrame {
        CallFrame {
            func_index,
            pc: 0,
            locals: (0..slot_count)
                .map(|_| new_cell(Value::undefined()))
                .collect(),
            upvalues: Vec::new(),
            this: Value::undefined(),
            captured_this: None,
            stack: OperandStack::with_capacity(16),
            span,
            is_construct: false,
            generator: None,
            try_stack: Vec::new(),
            pending_throw: None,
        }
    }
}

/// Construct a fresh cell wrapping `v`.
pub fn new_cell(v: Value) -> Cell {
    Rc::new(RefCell::new(v))
}
