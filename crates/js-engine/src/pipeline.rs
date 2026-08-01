//! The end-to-end pipeline: source → AST → bytecode → (interpret | JIT | AOT).

use crate::config::{EngineConfig, ExecutionMode};
use crate::module::CompiledModule;
use js_diagnostics::DiagnosticReport;
use js_runtime::context::RealmContext;
use js_runtime::value::Value;
use js_syntax::{ProgramKind, SourceFile};
use std::fmt;
use std::sync::Arc;

/// Opaque native artifact produced by the JIT/AOT backends.
#[cfg(any(feature = "jit", feature = "aot"))]
pub struct NativeArtifact {
    pub kind: &'static str,
}

/// The result of running a program.
#[derive(Debug)]
pub struct RunResult {
    /// The completion value of the script (top-level return).
    pub value: Value,
    pub mode: ExecutionMode,
}

/// The single failure taxonomy used by every public execution API.
#[derive(Debug)]
pub enum EngineError {
    Compile(DiagnosticReport),
    Exception(js_vm::JsException),
    Fault(js_vm::EngineFault),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Compile(report) => report.fmt(f),
            EngineError::Exception(error) => error.fmt(f),
            EngineError::Fault(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<js_vm::RuntimeError> for EngineError {
    fn from(error: js_vm::RuntimeError) -> Self {
        match error {
            js_vm::RuntimeError::Exception(error) => EngineError::Exception(error),
            js_vm::RuntimeError::Fault(error) => EngineError::Fault(error),
        }
    }
}

/// Structured execution outcome for hosts that prefer matching over `Result`.
/// It carries exactly the same [`EngineError`] used by [`Engine::run`].
#[derive(Debug)]
pub enum ExecutionOutcome {
    Completed(Value),
    Failed(EngineError),
}

/// Backwards-compatible type name for the conformance runner and embedders.
pub type ExecOutcome = ExecutionOutcome;

/// The top-level engine handle.
pub struct Engine {
    config: EngineConfig,
    ctx: RealmContext,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Engine {
        Engine {
            config,
            ctx: RealmContext::fresh(),
        }
    }

    /// A default-config engine (interpret mode, fresh realm).
    pub fn default_interpreter() -> Engine {
        Engine::new(EngineConfig::default())
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// Parse + compile a classic script from an in-memory source.
    pub fn compile(&self, src: &str) -> Result<CompiledModule, DiagnosticReport> {
        self.compile_named("<eval>", src, ProgramKind::Script)
    }

    /// Parse + compile while retaining the source name and syntactic goal.
    pub fn compile_named(
        &self,
        name: impl Into<String>,
        src: &str,
        kind: ProgramKind,
    ) -> Result<CompiledModule, DiagnosticReport> {
        let source = Arc::new(SourceFile::new(name, Arc::<str>::from(src)));
        let sess = js_parser::ParseSess::from_shared(source.clone());
        let program = js_parser::Parser::new(&sess)
            .parse(kind)
            .map_err(|diagnostics| DiagnosticReport::new(source.clone(), diagnostics))?;
        let bytecode = js_bytecode::compile_program_with_source(&program, source.clone())
            .map_err(|diagnostics| DiagnosticReport::new(source.clone(), diagnostics))?;
        Ok(CompiledModule::new(source, bytecode))
    }

    /// Parse + compile + run.
    pub fn run(&mut self, src: &str) -> Result<RunResult, EngineError> {
        self.run_named("<eval>", src, ProgramKind::Script)
    }

    pub fn run_named(
        &mut self,
        name: impl Into<String>,
        src: &str,
        kind: ProgramKind,
    ) -> Result<RunResult, EngineError> {
        let compiled = self
            .compile_named(name, src, kind)
            .map_err(EngineError::Compile)?;
        let value = match self.config.mode {
            ExecutionMode::Interpret | ExecutionMode::AstWalk => {
                let mut interp = js_vm::Interpreter::new(self.ctx_realm_clone());
                interp
                    .run_module_report(&compiled.bytecode)
                    .map_err(EngineError::from)?
            }
            ExecutionMode::Jit => self.run_jit(&compiled)?,
            ExecutionMode::Aot => {
                self.run_aot(&compiled)?;
                Value::undefined()
            }
        };
        Ok(RunResult {
            value,
            mode: self.config.mode,
        })
    }

    /// Install the test262 harness globals (`assert`, `Test262Error`, `$DONE`)
    /// into this engine's realm. Persist for the life of the engine (the realm
    /// is shared across `execute` calls). Idempotent.
    pub fn install_test262_harness(&mut self) {
        let mut realm = self.ctx.realm.borrow_mut();
        js_vm::builtins::install_test262_harness(&mut realm.globals);
    }

    /// Parse + compile + execute with the same failure taxonomy as [`Self::run`].
    pub fn execute(&mut self, src: &str) -> ExecutionOutcome {
        match self.run(src) {
            Ok(result) => ExecutionOutcome::Completed(result.value),
            Err(error) => ExecutionOutcome::Failed(error),
        }
    }

    pub fn execute_named(
        &mut self,
        name: impl Into<String>,
        src: &str,
        kind: ProgramKind,
    ) -> ExecutionOutcome {
        match self.run_named(name, src, kind) {
            Ok(result) => ExecutionOutcome::Completed(result.value),
            Err(error) => ExecutionOutcome::Failed(error),
        }
    }

    fn ctx_realm_clone(&self) -> RealmContext {
        // Each run gets its own interpreter + realm view. The realm itself is
        // shared via Rc inside RealmContext; clone the handle.
        RealmContext {
            realm: self.ctx.realm.clone(),
        }
    }

    fn run_jit(&self, compiled: &CompiledModule) -> Result<Value, EngineError> {
        #[cfg(feature = "jit")]
        {
            let compiler = js_codegen::JitCompiler::for_host();
            let _jit = compiler
                .compile(&compiled.bytecode)
                .map_err(|e| self.backend_fault(compiled, format!("{e:?}")))?;
            // TODO: invoke the native entry for `<main>` with a runtime trampoline.
            return Ok(Value::undefined());
        }
        #[cfg(not(feature = "jit"))]
        Err(self.backend_fault(
            compiled,
            "JIT backend not enabled (rebuild with `--features jit`)",
        ))
    }

    fn run_aot(&self, compiled: &CompiledModule) -> Result<(), EngineError> {
        #[cfg(feature = "aot")]
        {
            let triple = self
                .config
                .target_triple
                .clone()
                .unwrap_or_else(|| std::env::consts::ARCH.to_string());
            let compiler = js_codegen::AotCompiler::new(triple);
            let artifact = compiler
                .compile(&compiled.bytecode)
                .map_err(|e| self.backend_fault(compiled, format!("{e:?}")))?;
            let _bytes = artifact
                .finish()
                .map_err(|e| self.backend_fault(compiled, format!("{e:?}")))?;
            return Ok(());
        }
        #[cfg(not(feature = "aot"))]
        {
            Err(self.backend_fault(
                compiled,
                "AOT backend not enabled (rebuild with `--features aot`)",
            ))
        }
    }

    fn backend_fault(&self, compiled: &CompiledModule, message: impl Into<String>) -> EngineError {
        let span = compiled.bytecode.main.span;
        EngineError::Fault(js_vm::EngineFault::new(
            message,
            Some(compiled.source.clone()),
            vec![js_vm::RuntimeFrame {
                function: compiled.bytecode.main.name.clone(),
                span,
            }],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_empty_script() {
        let mut engine = Engine::default_interpreter();
        let result = engine.run("").expect("empty script runs");
        assert!(result.value.is_undefined());
    }
}
