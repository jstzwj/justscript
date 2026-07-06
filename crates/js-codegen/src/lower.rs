//! Shared bytecode → Cranelift IR lowering.
//!
//! [`lower_function`] builds the CLIR for one bytecode function. Both the JIT
//! and AOT backends call it; the only difference is the *module* the function
//! is declared in (`cranelift_jit::JITModule` vs `cranelift_object::ObjectModule`).
//!
//! **Skeleton state:** the entry point exists but emits no CLIR yet.

use js_bytecode::BytecodeFunction;

/// The result of lowering one function: opaque to backends, which only care
/// that it can be declared/defined in their module.
pub struct LoweredFunction {
    /// The function's source name (for debug info / symbol naming).
    pub name: String,
    /// Number of integer-sized value parameters in the lowered signature.
    pub param_count: u16,
}

/// Lower `func` to Cranelift IR, ready to define in a Cranelift module.
///
/// TODO: build the `FunctionBuilder`, walk `func.code`, and emit CLIR per
/// opcode, calling out to runtime helpers via C ABI for object/call ops.
pub fn lower_function(func: &BytecodeFunction) -> LoweredFunction {
    LoweredFunction {
        name: func.name.clone(),
        param_count: func.param_count,
    }
}
