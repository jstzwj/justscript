//! The JIT backend: compile bytecode functions to native code in-process and
//! obtain callable function pointers.
//!
//! **Skeleton state:** types and entry points are in place; actual
//! `cranelift_jit::JITModule` usage lands incrementally.

#![allow(unused_imports)]

use crate::lower::{lower_function, LoweredFunction};
use js_bytecode::BytecodeModule;

// Cranelift imports — intentionally unused in the skeleton (bodies are
// `todo!()` to avoid coupling the skeleton to a specific Cranelift version).
use cranelift_frontend::FunctionBuilder;
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Linkage, Module};

/// Errors that can occur during JIT compilation.
#[derive(Debug)]
pub enum JitError {
    Codegen(String),
    /// A bytecode opcode is not yet supported by the lowering.
    Unsupported(String),
}

/// A JIT-compiled module: holds the `JITModule` (keeping the code alive) and
/// the symbol table mapping function index → native entry point.
pub struct JitModule {
    /// Human-readable name for diagnostics.
    pub name: String,
    // TODO: hold the `JITModule` and a Vec<*mut u8> of entry points.
}

/// The JIT compiler driver.
pub struct JitCompiler {
    // TODO: hold the target ISA + shared runtime symbol table.
}

impl JitCompiler {
    /// Create a JIT compiler targeting the host.
    pub fn for_host() -> JitCompiler {
        JitCompiler {}
    }

    /// Compile an entire module and return a [`JitModule`] that owns the code.
    pub fn compile(&self, module: &BytecodeModule) -> Result<JitModule, JitError> {
        // TODO: for each function, lower_function() + declare + define.
        let _ = lower_function(&module.main);
        Ok(JitModule {
            name: "<main>".to_string(),
        })
    }
}

impl JitModule {
    /// Look up the native entry pointer for a function by index.
    pub fn entry(&self, _func_index: usize) -> Result<*mut u8, JitError> {
        todo!("JitModule::entry — wire cranelift_jit::JITModule::get_finalized_function")
    }
}
