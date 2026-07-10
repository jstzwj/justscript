//! The JustScript bytecode interpreter.
//!
//! Executes a [`js_bytecode::BytecodeModule`] against a [`js_runtime`] realm.
//! This is the *baseline* execution backend; the JIT and AOT backends in
//! [`js_codegen`] are faster paths for hot code.
//!
//! **Skeleton state:** the dispatch loop and frame/stack types are in place;
//! opcode semantics land incrementally.

pub mod builtins;
pub mod frame;
pub mod interp;
pub mod regex;
pub mod stack;

pub use frame::CallFrame;
pub use interp::{InterpError, Interpreter};
pub use stack::OperandStack;
