//! Cranelift ISA / target configuration, shared by the JIT and AOT backends.
//!
//! Builds the host ISA (for JIT) or a chosen triple (for AOT) with the
//! project's preferred codegen flags. **Skeleton:** returns a configured
//! builder type once the cranelift API is wired in.

#![allow(unused_imports)]

use cranelift_codegen::settings;
use cranelift_native;

/// A configured Cranelift target (settings + ISA builder), ready to finish.
pub struct Target {
    pub name: String,
}

/// Detect the host target. Used by the JIT backend.
pub fn host_target() -> Target {
    // TODO: cranelift_native::builder() + flags.
    Target {
        name: std::env::consts::ARCH.to_string(),
    }
}

/// A target chosen by name, for the AOT backend.
pub fn named_target(triple: &str) -> Target {
    Target {
        name: triple.to_string(),
    }
}
