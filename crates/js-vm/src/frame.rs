//! A single call frame: the function being executed, its program counter,
//! locals and operand stack.

use crate::stack::OperandStack;
use js_runtime::value::Value;
use js_syntax::Span;

pub struct CallFrame {
    /// Index of the executing function within the module's function table
    /// (0 = top-level `<main>`).
    pub func_index: usize,
    pub pc: usize,
    pub locals: Vec<Value>,
    pub stack: OperandStack,
    /// Source span of the *currently executing* instruction, for backtraces.
    pub span: Span,
}

impl CallFrame {
    pub fn new(func_index: usize, slot_count: u16, span: Span) -> CallFrame {
        CallFrame {
            func_index,
            pc: 0,
            locals: vec![Value::undefined(); slot_count as usize],
            stack: OperandStack::with_capacity(16),
            span,
        }
    }
}
