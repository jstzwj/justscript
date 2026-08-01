//! The compiled-module abstraction: a single type holding whatever a given
//! [`crate::ExecutionMode`] produced.

use js_bytecode::BytecodeModule;
use js_syntax::SourceFile;
use std::sync::Arc;

/// The artifact produced by compiling a source program.
///
/// - Always carries the parsed AST and the bytecode (the universal IR).
/// - Carries native code only when JIT/AOT ran successfully.
pub struct CompiledModule {
    pub source: Arc<SourceFile>,
    pub bytecode: BytecodeModule,
    #[cfg(feature = "jit")]
    pub native: Option<crate::pipeline::NativeArtifact>,
}

impl CompiledModule {
    pub fn new(source: Arc<SourceFile>, bytecode: BytecodeModule) -> CompiledModule {
        CompiledModule {
            source,
            bytecode,
            #[cfg(feature = "jit")]
            native: None,
        }
    }
}
