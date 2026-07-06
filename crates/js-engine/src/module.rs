//! The compiled-module abstraction: a single type holding whatever a given
//! [`crate::ExecutionMode`] produced.

use js_bytecode::BytecodeModule;

/// The artifact produced by compiling a source program.
///
/// - Always carries the parsed AST and the bytecode (the universal IR).
/// - Carries native code only when JIT/AOT ran successfully.
pub struct CompiledModule {
    pub bytecode: BytecodeModule,
    #[cfg(feature = "jit")]
    pub native: Option<crate::pipeline::NativeArtifact>,
}

impl CompiledModule {
    pub fn new(bytecode: BytecodeModule) -> CompiledModule {
        CompiledModule {
            bytecode,
            #[cfg(feature = "jit")]
            native: None,
        }
    }
}
