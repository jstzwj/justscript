//! The JustScript engine: the single top-level API users call.
//!
//! [`Engine`] ties the pipeline together:
//! ```text
//!   source --js_parser--> Program
//!          --js_bytecode--> BytecodeModule
//!          --[Interp]: js_vm::Interpreter
//!           [Jit]:    js_codegen::jit
//!           [Aot]:    js_codegen::aot
//! ```
//!
//! The execution mode is chosen per [`Engine`] via [`EngineConfig`], so the
//! same source can be run interpreted, JIT-compiled or AOT-compiled with no
//! code changes at the call site.

pub mod config;
pub mod module;
pub mod pipeline;

pub use config::{EngineConfig, ExecutionMode};
pub use module::CompiledModule;
pub use pipeline::{Engine, ExecOutcome, RunResult};

pub use js_runtime::value::Value;
