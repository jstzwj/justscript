//! The AOT backend: lower bytecode functions to a native object file on disk
//! via `cranelift_object`.
//!
//! **Skeleton state:** types and entry points are in place.

#![allow(unused_imports)]

use crate::lower::lower_function;
use js_bytecode::BytecodeModule;

use cranelift_module::{Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};

/// Errors that can occur during AOT compilation.
#[derive(Debug)]
pub enum AotError {
    Codegen(String),
    Io(String),
}

/// An in-progress object-file artifact: functions get added, then finalized
/// to bytes.
pub struct ObjectArtifact {
    pub triple: String,
}

/// The AOT compiler driver.
pub struct AotCompiler {
    pub triple: String,
}

impl AotCompiler {
    pub fn new(triple: impl Into<String>) -> AotCompiler {
        AotCompiler {
            triple: triple.into(),
        }
    }

    /// Compile a module into an [`ObjectArtifact`].
    pub fn compile(&self, module: &BytecodeModule) -> Result<ObjectArtifact, AotError> {
        let _ = lower_function(&module.main);
        Ok(ObjectArtifact {
            triple: self.triple.clone(),
        })
    }
}

impl ObjectArtifact {
    /// Finalize and emit the object file as raw bytes.
    pub fn finish(self) -> Result<Vec<u8>, AotError> {
        todo!("ObjectArtifact::finish — wire cranelift_object::ObjectModule::finish + emit")
    }
}
