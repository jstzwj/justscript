//! Structural bytecode verification.
//!
//! Bytecode is an internal trust boundary: the interpreter and native
//! backends may assume that table indices and control-flow targets have been
//! checked once before execution. Semantic JavaScript errors never belong in
//! this layer; a verification failure is always an engine/compiler fault.

use crate::{BytecodeFunction, BytecodeModule, OperandKind};
use std::fmt;

/// Version of the in-memory bytecode contract. This is intentionally separate
/// from any future serialized file-format version.
pub const BYTECODE_FORMAT_VERSION: u32 = 12;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifyError {
    pub function: usize,
    pub pc: Option<usize>,
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pc {
            Some(pc) => write!(f, "function {} pc {}: {}", self.function, pc, self.message),
            None => write!(f, "function {}: {}", self.function, self.message),
        }
    }
}

/// Verify all structural invariants required by every execution backend.
pub fn verify_module(module: &BytecodeModule) -> Result<(), Vec<VerifyError>> {
    let mut errors = Vec::new();
    verify_function(module, &module.main, 0, &mut errors);
    for (index, function) in module.functions.iter().enumerate() {
        verify_function(module, function, index + 1, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn verify_function(
    module: &BytecodeModule,
    function: &BytecodeFunction,
    function_id: usize,
    errors: &mut Vec<VerifyError>,
) {
    if function.code.len() != function.source_map.len() {
        errors.push(VerifyError {
            function: function_id,
            pc: None,
            message: format!(
                "instruction/source-map length mismatch: {} != {}",
                function.code.len(),
                function.source_map.len()
            ),
        });
    }
    if function.param_count > function.locals.slot_count() {
        errors.push(VerifyError {
            function: function_id,
            pc: None,
            message: format!(
                "parameter count {} exceeds local slot count {}",
                function.param_count,
                function.locals.slot_count()
            ),
        });
    }
    if function.upvalues.len() != function.upvalue_names.len() {
        errors.push(VerifyError {
            function: function_id,
            pc: None,
            message: format!(
                "upvalue/name length mismatch: {} != {}",
                function.upvalues.len(),
                function.upvalue_names.len()
            ),
        });
    }

    for (pc, instruction) in function.code.iter().enumerate() {
        let operand = instruction.operand as usize;
        let valid = match instruction.op.operand_kind() {
            OperandKind::None => instruction.operand == 0,
            OperandKind::Constant => operand < module.constants.len(),
            OperandKind::Local => operand < function.locals.slot_count() as usize,
            OperandKind::Upvalue => operand < function.upvalues.len(),
            // Function zero is the module entry point and is never a closure.
            OperandKind::Function => operand > 0 && operand <= module.functions.len(),
            // A branch to code.len() is a valid function exit boundary.
            OperandKind::JumpTarget => operand <= function.code.len(),
            OperandKind::ArgumentCount => true,
            OperandKind::Handler => operand < function.handlers.len(),
            OperandKind::ClassField => true,
        };
        if !valid {
            errors.push(VerifyError {
                function: function_id,
                pc: Some(pc),
                message: format!(
                    "invalid {:?} operand {} for {:?}",
                    instruction.op.operand_kind(),
                    instruction.operand,
                    instruction.op
                ),
            });
        }
    }

    for (index, handler) in function.handlers.iter().enumerate() {
        for (kind, target) in [("catch", handler.catch_pc), ("finally", handler.finally_pc)] {
            if target.is_some_and(|pc| pc as usize >= function.code.len()) {
                errors.push(VerifyError {
                    function: function_id,
                    pc: None,
                    message: format!("handler {index} has invalid {kind} target {target:?}"),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConstantPool, Instruction, Opcode};
    use js_syntax::Span;

    #[test]
    fn accepts_compiler_output() {
        let program =
            js_syntax::ast::Program::new(Span::DUMMY, js_syntax::ast::ProgramKind::Script, vec![]);
        let module = crate::compile_program(&program).expect("compile");
        verify_module(&module).expect("valid bytecode");
    }

    #[test]
    fn rejects_out_of_range_constant() {
        let mut function = BytecodeFunction::new(Span::DUMMY, "<main>", 0);
        function.emit(Instruction::new(Opcode::LdaConst, 7));
        function.emit_bare(Opcode::Return);
        let module = BytecodeModule::new(ConstantPool::new(), function);
        let errors = verify_module(&module).expect_err("invalid constant index");
        assert!(errors[0].message.contains("Constant"));
    }

    #[test]
    fn rejects_instruction_source_map_mismatch() {
        let mut function = BytecodeFunction::new(Span::DUMMY, "<main>", 0);
        function.code.push(Instruction::bare(Opcode::Return));
        let module = BytecodeModule::new(ConstantPool::new(), function);
        let errors = verify_module(&module).expect_err("missing source-map entry");
        assert!(errors[0].message.contains("source-map length mismatch"));
    }
}
