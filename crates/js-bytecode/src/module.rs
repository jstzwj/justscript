//! Bytecode containers: functions and modules.

use crate::constant::ConstantPool;
use crate::local::LocalTable;
use crate::opcode::Instruction;
use js_syntax::SourceFile;
use js_syntax::Span;
use std::sync::Arc;

/// A compiled description of one captured variable (closure upvalue).
///
/// At function-creation time (`LdaFunction`), each upvalue is resolved to a
/// concrete cell: if `is_local`, the cell at `index` in the *enclosing* frame's
/// locals; otherwise the cell at `index` in the enclosing function's already-
/// captured upvalues (for nesting deeper than one level).
#[derive(Debug, Clone, Copy, Default)]
pub struct UpvalueSpec {
    pub is_local: bool,
    pub index: u16,
}

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
    /// Source span for each instruction in [`Self::code`].
    pub source_map: Vec<Span>,
    /// Local slot table (names → slot index).
    pub locals: LocalTable,
    /// Upvalue descriptors, in capture order; indexed by `LdaUpvalue`/`StaUpvalue`.
    pub upvalues: Vec<UpvalueSpec>,
    /// True for arrow functions, which capture `this` lexically.
    pub is_arrow: bool,
    /// True for `function*`: calling it produces a generator object instead of
    /// running the body. `yield` inside is a `Yield` opcode.
    pub is_generator: bool,
    /// Exception-handler specs, indexed by `TryBegin`'s operand.
    pub handlers: Vec<HandlerSpec>,
}

/// A compiled `try` handler: where to jump on a caught exception, and where to
/// run the `finally` (either may be absent). Resolved at compile time.
#[derive(Debug, Clone, Copy, Default)]
pub struct HandlerSpec {
    /// `catch` clause entry pc, if any.
    pub catch_pc: Option<u16>,
    /// `finally` clause entry pc, if any.
    pub finally_pc: Option<u16>,
}

impl BytecodeFunction {
    pub fn new(span: Span, name: impl Into<String>, param_count: u16) -> BytecodeFunction {
        BytecodeFunction {
            span,
            name: name.into(),
            param_count,
            code: Vec::new(),
            source_map: Vec::new(),
            locals: LocalTable::new(param_count),
            upvalues: Vec::new(),
            is_arrow: false,
            is_generator: false,
            handlers: Vec::new(),
        }
    }

    pub fn emit(&mut self, ins: Instruction) {
        self.code.push(ins);
        self.source_map.push(Span::DUMMY);
    }

    /// Emit an instruction with an explicit source location.
    pub fn emit_at(&mut self, ins: Instruction, span: Span) {
        self.code.push(ins);
        self.source_map.push(span);
    }

    /// Emit a bare (no-operand) instruction.
    pub fn emit_bare(&mut self, op: crate::opcode::Opcode) {
        self.emit(Instruction::bare(op));
    }

    pub fn emit_bare_at(&mut self, op: crate::opcode::Opcode, span: Span) {
        self.emit_at(Instruction::bare(op), span);
    }

    /// Associate unannotated instructions emitted since `start_pc` with
    /// `span`. Nested compiler calls annotate their own instructions first, so
    /// their more precise spans are preserved.
    pub fn annotate_since(&mut self, start_pc: usize, span: Span) {
        for mapped in &mut self.source_map[start_pc..] {
            if mapped.is_dummy() {
                *mapped = span;
            }
        }
    }

    pub fn source_span(&self, pc: usize) -> Span {
        self.source_map.get(pc).copied().unwrap_or(self.span)
    }

    /// Current instruction index — useful to backpatch jump targets.
    pub fn here(&self) -> u16 {
        self.code.len() as u16
    }
}

/// A compiled module: top-level function + nested functions + shared constants.
#[derive(Debug, Default)]
pub struct BytecodeModule {
    /// The source this module was compiled from, retained for runtime reports.
    pub source: Option<Arc<SourceFile>>,
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
            source: None,
            constants,
            main,
            functions: Vec::new(),
        }
    }
}
