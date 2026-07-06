//! The per-frame operand stack.

use js_runtime::value::Value;

/// A growable stack of [`Value`]s used as the bytecode VM's operand stack.
#[derive(Default)]
pub struct OperandStack {
    slots: Vec<Value>,
}

impl OperandStack {
    pub fn with_capacity(n: usize) -> OperandStack {
        OperandStack {
            slots: Vec::with_capacity(n),
        }
    }

    pub fn push(&mut self, v: Value) {
        self.slots.push(v);
    }

    pub fn pop(&mut self) -> Value {
        self.slots.pop().unwrap_or_default()
    }

    /// Peek the top without popping.
    pub fn peek(&self) -> &Value {
        self.slots.last().unwrap_or_else(|| {
            // Operand stack underflow is a VM bug, not user error.
            panic!("operand stack underflow")
        })
    }

    pub fn dup(&mut self) {
        if let Some(v) = self.slots.last().cloned() {
            self.slots.push(v);
        }
    }

    pub fn depth(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}
