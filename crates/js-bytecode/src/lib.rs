//! The JustScript bytecode: a compact, stack-based instruction set that is the
//! shared input format for both the interpreter ([`js_vm`]) and the JIT/AOT
//! lowering ([`js_codegen`]).
//!
//! Pipeline role:
//! ```text
//! js_parser::Program --compiler--> BytecodeModule --[vm]|--[codegen]--> result
//! ```
//!
//! The opcode set is deliberately small and orthogonal so that each opcode
//! maps cleanly onto a small fragment of Cranelift IR.

pub mod compiler;
pub mod constant;
pub mod local;
pub mod module;
pub mod opcode;
pub mod verify;

pub use compiler::{
    compile_eval_program_with_source, compile_program, compile_program_with_source,
};
pub use constant::ConstantPool;
pub use local::LocalTable;
pub use module::{BytecodeFunction, BytecodeModule, DEFAULT_EXPORT_LOCAL};
pub use opcode::{Instruction, Opcode, OperandKind};
pub use verify::{verify_module, VerifyError, BYTECODE_FORMAT_VERSION};
