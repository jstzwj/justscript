//! Bytecode containers: functions and modules.

use crate::constant::ConstantPool;
use crate::local::LocalTable;
use crate::opcode::Instruction;
use js_syntax::Span;

/// One compiled function: its instructions, locals, constants and metadata.
#[derive(Debug, Default)]
pub struct BytecodeFunction {
    /// The span of the function in the source (for backtraces/debug info).
    pub span: Span,
    /// A user-visible name (top-level is `<main>`).
    pub name: String,
    /// Parameter count.
    pub param_count: u16,
    /// The flat instruction list. Jump operands are indices into this vec.
    pub code: Vec<Instruction>,
    /// Local slot table (names → slot index).
    pub locals: LocalTable,
}

impl BytecodeFunction {
    pub fn new(span: Span, name: impl Into<String>, param_count: u16) -> BytecodeFunction {
        BytecodeFunction {
            span,
            name: name.into(),
            param_count,
            code: Vec::new(),
            locals: LocalTable::new(param_count),
        }
    }

    pub fn emit(&mut self, ins: Instruction) {
        self.code.push(ins);
    }

    /// Emit a bare (no-operand) instruction.
    pub fn emit_bare(&mut self, op: crate::opcode::Opcode) {
        self.emit(Instruction::bare(op));
    }

    /// Current instruction index — useful to backpatch jump targets.
    pub fn here(&self) -> u16 {
        self.code.len() as u16
    }
}

/// A compiled module: top-level function + nested functions + shared constants.
#[derive(Debug, Default)]
pub struct BytecodeModule {
    /// Constant pool shared across all functions in the module.
    pub constants: ConstantPool,
    /// The top-level (script-body) function.
    pub main: BytecodeFunction,
    /// Nested function declarations, in discovery order.
    pub functions: Vec<BytecodeFunction>,
}

impl BytecodeModule {
    pub fn new(constants: ConstantPool, main: BytecodeFunction) -> BytecodeModule {
        BytecodeModule {
            constants,
            main,
            functions: Vec::new(),
        }
    }
}
