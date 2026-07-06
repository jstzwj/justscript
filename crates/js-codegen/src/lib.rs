//! The JustScript native backends, both built on [Cranelift].
//!
//! - With the `jit` feature: [`jit`] compiles bytecode functions to native
//!   code *in-process* and returns callable function pointers
//!   (`cranelift_jit::JITModule`).
//! - With the `aot` feature: [`aot`] lowers to native object files on disk
//!   (`cranelift_object::object::Object`).
//!
//! Both backends share [`lower`], which turns a [`js_bytecode::BytecodeFunction`]
//! into a Cranelift IR function. Runtime helpers (allocation, calls, property
//! access) are exposed through a stable C ABI so JIT'd code can call back into
//! [`js_runtime`] without re-deriving them.
//!
//! **Skeleton state:** modules, types and lowering entry points are in place;
//! the CLIR emission is filled in incrementally.
//!
//! [Cranelift]: https://github.com/bytecodealliance/wasmtime/tree/main/cranelift

#[cfg(any(feature = "jit", feature = "aot"))]
pub mod isa;
#[cfg(feature = "jit")]
pub mod jit;
#[cfg(feature = "aot")]
pub mod aot;

pub mod lower;

#[cfg(feature = "jit")]
pub use jit::{JitCompiler, JitError, JitModule};
#[cfg(feature = "aot")]
pub use aot::{AotCompiler, AotError, ObjectArtifact};
