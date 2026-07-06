//! Engine configuration.

/// Which backend a program is run with.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub enum ExecutionMode {
    /// Parse + AST-walk (TODO): the slowest but simplest path.
    AstWalk,
    /// Parse + compile to bytecode + interpret. The default baseline.
    #[default]
    Interpret,
    /// Parse + compile to bytecode + JIT-compile hot functions.
    Jit,
    /// Ahead-of-time compile to an object file (no execution).
    Aot,
}

/// Engine-wide configuration.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub mode: ExecutionMode,
    /// For AOT: the target triple to emit for (`None` = host).
    pub target_triple: Option<String>,
    /// Whether to print diagnostics to stderr as they're produced.
    pub emit_diagnostics: bool,
}

impl Default for EngineConfig {
    fn default() -> EngineConfig {
        EngineConfig {
            mode: ExecutionMode::Interpret,
            target_triple: None,
            emit_diagnostics: true,
        }
    }
}
