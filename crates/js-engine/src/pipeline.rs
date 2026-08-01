//! The end-to-end pipeline: source → AST → bytecode → (interpret | JIT | AOT).

use crate::config::{EngineConfig, ExecutionMode};
use crate::module::CompiledModule;
use js_bytecode::BytecodeModule;
use js_diagnostics::DiagResult;
use js_runtime::context::RealmContext;
use js_runtime::value::Value;
use js_syntax::ast::Program;
use js_syntax::ProgramKind;

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

/// A structured execution outcome, distinguishing the failure modes the runtime
/// conformance runner cares about: a parse/compile error (a *front-end* miss),
/// a real JavaScript throw (a test assertion failure or an *expected* negative
/// exception), or an internal VM limitation (an unimplemented opcode / VM bug —
/// not a meaningful pass or fail).
#[derive(Debug)]
pub enum ExecOutcome {
    /// Ran to completion with this value.
    Ok(Value),
    /// Parse or bytecode-compile error (front-end rejected the source).
    CompileError(Vec<js_diagnostics::Diagnostic>),
    /// A JavaScript `throw` reached the top level (assertion failure, or the
    /// expected exception of a `negative: runtime` test).
    Threw(Value),
    /// The VM could not execute the program (unimplemented feature / VM bug).
    Internal(String),
}

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

    /// Parse + compile a source string.
    pub fn compile(&self, src: &str) -> DiagResult<CompiledModule> {
        let program = parse(src, ProgramKind::Script)?;
        let bytecode = js_bytecode::compile_program(&program)?;
        Ok(CompiledModule::new(bytecode))
    }

    /// Parse + compile + run.
    pub fn run(&mut self, src: &str) -> DiagResult<RunResult> {
        let compiled = self.compile(src)?;
        let value = match self.config.mode {
            ExecutionMode::Interpret | ExecutionMode::AstWalk => {
                let mut interp = js_vm::Interpreter::new(self.ctx_realm_clone());
                interp.run_module(&compiled.bytecode).map_err(|e| {
                    vec![js_diagnostics::Diagnostic::error(
                        js_syntax::Span::DUMMY,
                        format!("{e}"),
                    )]
                })?
            }
            ExecutionMode::Jit => self.run_jit(&compiled.bytecode)?,
            ExecutionMode::Aot => {
                self.run_aot(&compiled.bytecode)?;
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

    /// Parse + compile + execute, returning a structured outcome that separates
    /// compile errors, JavaScript throws, and internal VM limitations. Drives
    /// the interpret backend only (JIT/AOT don't run programs yet).
    pub fn execute(&mut self, src: &str) -> ExecOutcome {
        let compiled = match self.compile(src) {
            Ok(c) => c,
            Err(diags) => return ExecOutcome::CompileError(diags),
        };
        let mut interp = js_vm::Interpreter::new(self.ctx_realm_clone());
        match interp.run_module(&compiled.bytecode) {
            Ok(v) => ExecOutcome::Ok(v),
            Err(js_vm::InterpError::Throw(v)) => ExecOutcome::Threw(v),
            Err(js_vm::InterpError::Internal(msg)) => ExecOutcome::Internal(msg),
        }
    }

    fn ctx_realm_clone(&self) -> RealmContext {
        // Each run gets its own interpreter + realm view. The realm itself is
        // shared via Rc inside RealmContext; clone the handle.
        RealmContext {
            realm: self.ctx.realm.clone(),
        }
    }

    fn run_jit(&self, _module: &BytecodeModule) -> DiagResult<Value> {
        #[cfg(feature = "jit")]
        {
            let compiler = js_codegen::JitCompiler::for_host();
            let _jit = compiler.compile(_module).map_err(|e| {
                vec![js_diagnostics::Diagnostic::error(
                    js_syntax::Span::DUMMY,
                    format!("{e:?}"),
                )]
            })?;
            // TODO: invoke the native entry for `<main>` with a runtime trampoline.
            return Ok(Value::undefined());
        }
        #[cfg(not(feature = "jit"))]
        Err(vec![js_diagnostics::Diagnostic::error(
            js_syntax::Span::DUMMY,
            "JIT backend not enabled (rebuild with `--features jit`)",
        )])
    }

    fn run_aot(&self, module: &BytecodeModule) -> DiagResult<()> {
        #[cfg(feature = "aot")]
        {
            let triple = self
                .config
                .target_triple
                .clone()
                .unwrap_or_else(|| std::env::consts::ARCH.to_string());
            let compiler = js_codegen::AotCompiler::new(triple);
            let artifact = compiler.compile(module).map_err(|e| {
                vec![js_diagnostics::Diagnostic::error(
                    js_syntax::Span::DUMMY,
                    format!("{e:?}"),
                )]
            })?;
            let _bytes = artifact.finish().map_err(|e| {
                vec![js_diagnostics::Diagnostic::error(
                    js_syntax::Span::DUMMY,
                    format!("{e:?}"),
                )]
            })?;
            return Ok(());
        }
        #[cfg(not(feature = "aot"))]
        {
            let _ = module;
            Err(vec![js_diagnostics::Diagnostic::error(
                js_syntax::Span::DUMMY,
                "AOT backend not enabled (rebuild with `--features aot`)",
            )])
        }
    }
}

fn parse(src: &str, kind: ProgramKind) -> DiagResult<Program> {
    match kind {
        ProgramKind::Script => js_parser::parse_script(src),
        ProgramKind::Module => js_parser::parse_module(src),
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
