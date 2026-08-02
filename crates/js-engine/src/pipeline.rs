//! The end-to-end pipeline: source → AST → bytecode → (interpret | JIT | AOT).

use crate::config::{EngineConfig, ExecutionMode};
use crate::module::{
    analyze_module, fresh_module_cells, CompiledModule, DynamicResolution, ExportEntry,
    ImportedName, ModuleError, ModuleGraph, ModuleLoader, ModuleStatus, RuntimeModule,
};
use js_diagnostics::DiagnosticReport;
use js_runtime::context::RealmContext;
use js_runtime::value::Value;
use js_syntax::{ProgramKind, SourceMap};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::rc::Rc;
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
    Module(ModuleError),
    Exception(js_vm::JsException),
    Fault(js_vm::EngineFault),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::Compile(report) => report.fmt(f),
            EngineError::Module(error) => error.fmt(f),
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
    sources: RefCell<SourceMap>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Engine {
        Engine {
            config,
            ctx: RealmContext::fresh(),
            sources: RefCell::new(SourceMap::new()),
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
        let source = self.sources.borrow_mut().add(name, Arc::<str>::from(src));
        let sess = js_parser::ParseSess::from_shared(source.clone());
        let program = js_parser::Parser::new(&sess)
            .parse(kind)
            .map_err(|diagnostics| DiagnosticReport::new(source.clone(), diagnostics))?;
        let bytecode = js_bytecode::compile_program_with_source(&program, source.clone())
            .map_err(|diagnostics| DiagnosticReport::new(source.clone(), diagnostics))?;
        Ok(CompiledModule::new(source, program, bytecode))
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

    /// Resolve, load, link, instantiate and execute an ECMAScript module graph.
    pub fn run_module(
        &mut self,
        entry: &str,
        loader: &dyn ModuleLoader,
    ) -> Result<RunResult, EngineError> {
        if !matches!(
            self.config.mode,
            ExecutionMode::Interpret | ExecutionMode::AstWalk
        ) {
            return Err(EngineError::Module(ModuleError::Unsupported {
                module: entry.to_string(),
                feature: "module graphs currently require the interpreter backend".into(),
            }));
        }
        let entry_key = loader.resolve(None, entry).map_err(|message| {
            EngineError::Module(ModuleError::Resolve {
                referrer: None,
                specifier: entry.to_string(),
                message,
            })
        })?;
        let mut graph = ModuleGraph::default();
        let entry_index = self.load_module_graph(loader, &entry_key, &mut graph)?;
        instantiate_module_functions(&mut graph)?;
        link_module(&mut graph, entry_index).map_err(EngineError::Module)?;
        for index in 0..graph.modules.len() {
            if graph.modules[index].status == ModuleStatus::Unlinked {
                link_module(&mut graph, index).map_err(EngineError::Module)?;
            }
        }
        materialize_namespaces(&mut graph)?;

        // Keep bytecode owners separate from mutable graph state while the VM
        // dispatches functions across module boundaries.
        let owners: Vec<Rc<CompiledModule>> = graph
            .modules
            .iter()
            .map(|module| module.compiled.clone())
            .collect();
        let bytecodes: Vec<_> = owners.iter().map(|module| &module.bytecode).collect();
        let mut interpreter = js_vm::Interpreter::new(self.ctx_realm_clone());
        let module_locals = graph
            .modules
            .iter()
            .map(|module| module.locals.clone())
            .collect();
        let module_dependencies = graph
            .modules
            .iter()
            .map(|module| {
                module
                    .metadata
                    .requests
                    .iter()
                    .filter(|request| request.phase == js_syntax::ImportPhase::Eval)
                    .filter_map(|request| module.dependencies.get(&request.specifier).copied())
                    .collect()
            })
            .collect();
        interpreter.configure_module_graph(module_locals, module_dependencies);
        let dynamic_imports = graph
            .modules
            .iter()
            .map(|module| {
                module
                    .dynamic_dependencies
                    .iter()
                    .map(|(specifier, resolution)| {
                        let resolution = match resolution {
                            DynamicResolution::Resolved(index) => Ok(*index),
                            DynamicResolution::Unresolved(message) => Err(message.clone()),
                        };
                        (specifier.clone(), resolution)
                    })
                    .collect()
            })
            .collect();
        interpreter.configure_dynamic_imports(dynamic_imports);
        let value =
            ModuleEvaluator::new(&mut graph, &bytecodes, &mut interpreter).evaluate(entry_index)?;
        Ok(RunResult {
            value,
            mode: self.config.mode,
        })
    }

    fn load_module_graph(
        &self,
        loader: &dyn ModuleLoader,
        key: &str,
        graph: &mut ModuleGraph,
    ) -> Result<usize, EngineError> {
        if let Some(index) = graph.by_key.get(key) {
            return Ok(*index);
        }
        let source_text = loader.load(key).map_err(|message| {
            EngineError::Module(ModuleError::Load {
                module: key.to_string(),
                message,
            })
        })?;
        let compiled = Rc::new(
            self.compile_named(key, &source_text, ProgramKind::Module)
                .map_err(EngineError::Compile)?,
        );
        let metadata = analyze_module(&compiled).map_err(EngineError::Module)?;
        let requests = metadata.requests.clone();
        let dynamic_requests = metadata.dynamic_requests.clone();
        let index = graph.modules.len();
        graph.by_key.insert(key.to_string(), index);
        let namespace = Value::object(js_runtime::object::ObjectData::module_namespace(
            BTreeMap::new(),
        ));
        graph.modules.push(RuntimeModule {
            key: key.to_string(),
            locals: fresh_module_cells(&compiled.bytecode, &metadata),
            compiled,
            metadata,
            dependencies: Default::default(),
            dynamic_dependencies: Default::default(),
            namespace: Some(namespace.clone()),
            namespace_cell: js_runtime::value::Cell::initialized(namespace, false),
            deferred_namespace: None,
            status: ModuleStatus::Unlinked,
            pending_async_dependencies: 0,
            async_parent_modules: Vec::new(),
            async_evaluation_order: None,
            evaluation_value: None,
            evaluation_error: None,
            dynamic_import_waiters: Vec::new(),
        });

        for request in requests {
            if request.phase == js_syntax::ImportPhase::Source {
                return Err(EngineError::Module(ModuleError::Unsupported {
                    module: key.to_string(),
                    feature: format!("{:?} import phase", request.phase),
                }));
            }
            let resolved = loader
                .resolve(Some(key), &request.specifier)
                .map_err(|message| {
                    EngineError::Module(ModuleError::Resolve {
                        referrer: Some(key.to_string()),
                        specifier: request.specifier.clone(),
                        message,
                    })
                })?;
            let dependency = self.load_module_graph(loader, &resolved, graph)?;
            graph.modules[index]
                .dependencies
                .insert(request.specifier, dependency);
        }
        for specifier in dynamic_requests {
            let resolution = match loader.resolve(Some(key), &specifier) {
                Ok(resolved) => match self.load_module_graph(loader, &resolved, graph) {
                    Ok(dependency) => DynamicResolution::Resolved(dependency),
                    Err(error) => DynamicResolution::Unresolved(error.to_string()),
                },
                Err(message) => DynamicResolution::Unresolved(message),
            };
            graph.modules[index]
                .dynamic_dependencies
                .insert(specifier, resolution);
        }
        Ok(index)
    }

    /// Install the test262 harness globals (`assert`, `Test262Error`, `$DONE`)
    /// into this engine's realm. Persist for the life of the engine (the realm
    /// is shared across `execute` calls). Idempotent.
    pub fn install_test262_harness(&mut self) {
        let mut realm = self.ctx.realm.borrow_mut();
        realm.test262_done_called = false;
        js_vm::builtins::install_test262_harness(&mut realm.globals);
    }

    /// Whether `$DONE` was observed in this engine's dedicated Test262 realm.
    pub fn test262_done_called(&self) -> bool {
        self.ctx.realm.borrow().test262_done_called
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
                source: Some(compiled.source.clone()),
            }],
        ))
    }
}

fn instantiate_module_functions(graph: &mut ModuleGraph) -> Result<(), EngineError> {
    for (module_index, module) in graph.modules.iter_mut().enumerate() {
        for &(slot, function_id) in &module.compiled.bytecode.module_function_initializers {
            let value = js_vm::Interpreter::instantiate_module_function(
                &module.compiled.bytecode,
                module_index,
                function_id,
                &module.locals,
            )
            .map_err(|message| {
                EngineError::Module(ModuleError::Link {
                    module: module.key.clone(),
                    message,
                })
            })?;
            module.locals[usize::from(slot)].set(value).map_err(|_| {
                EngineError::Module(ModuleError::Link {
                    module: module.key.clone(),
                    message: format!("cannot initialize function slot {slot}"),
                })
            })?;
        }
    }
    Ok(())
}

fn link_module(graph: &mut ModuleGraph, index: usize) -> Result<(), ModuleError> {
    match graph.modules[index].status {
        ModuleStatus::Linked
        | ModuleStatus::Evaluating
        | ModuleStatus::EvaluatingAsync
        | ModuleStatus::Evaluated => return Ok(()),
        ModuleStatus::Linking => return Ok(()),
        ModuleStatus::Errored => {
            return Err(ModuleError::Link {
                module: graph.modules[index].key.clone(),
                message: "module is already in an errored state".into(),
            })
        }
        ModuleStatus::Unlinked => {}
    }
    graph.modules[index].status = ModuleStatus::Linking;

    let dependency_order: Vec<_> = graph.modules[index]
        .metadata
        .requests
        .iter()
        .filter_map(|request| {
            graph.modules[index]
                .dependencies
                .get(&request.specifier)
                .copied()
        })
        .collect();
    for dependency in dependency_order {
        if let Err(error) = link_module(graph, dependency) {
            graph.modules[index].status = ModuleStatus::Errored;
            return Err(error);
        }
    }

    let imports = graph.modules[index].metadata.imports.clone();
    let mut bindings = Vec::with_capacity(imports.len());
    for import in imports {
        let dependency = *graph.modules[index]
            .dependencies
            .get(&import.request)
            .ok_or_else(|| ModuleError::Link {
                module: graph.modules[index].key.clone(),
                message: format!("request `{}` was not loaded", import.request),
            })?;
        let cell = match import.imported {
            ImportedName::Name(name) => {
                let target = resolve_export(graph, dependency, &name, &mut HashSet::new())?
                    .ok_or_else(|| ModuleError::Link {
                        module: graph.modules[index].key.clone(),
                        message: format!(
                            "module `{}` does not export `{name}`",
                            graph.modules[dependency].key
                        ),
                    })?;
                js_runtime::value::Cell::immutable_import(target)
            }
            ImportedName::Namespace => {
                if import.phase == js_syntax::ImportPhase::Defer {
                    let namespace = module_namespace_deferred(graph, dependency)?;
                    js_runtime::value::Cell::immutable_import(js_runtime::value::Cell::initialized(
                        namespace, false,
                    ))
                } else {
                    let _ = module_namespace(graph, dependency)?;
                    js_runtime::value::Cell::immutable_import(
                        graph.modules[dependency].namespace_cell.clone(),
                    )
                }
            }
        };
        bindings.push((import.local_slot, cell));
    }
    for (slot, cell) in bindings {
        graph.modules[index].locals[slot] = cell;
    }
    // ModuleDeclarationInstantiation validates every indirect export even when
    // no importer happens to request it.
    let indirect_exports: Vec<String> = graph.modules[index]
        .metadata
        .exports
        .iter()
        .filter_map(|export| match export {
            ExportEntry::Indirect { exported, .. } => Some(exported.clone()),
            _ => None,
        })
        .collect();
    for exported in indirect_exports {
        if resolve_export(graph, index, &exported, &mut HashSet::new())?.is_none() {
            graph.modules[index].status = ModuleStatus::Errored;
            return Err(ModuleError::Link {
                module: graph.modules[index].key.clone(),
                message: format!("indirect export `{exported}` cannot be resolved"),
            });
        }
    }
    graph.modules[index].status = ModuleStatus::Linked;
    Ok(())
}

fn resolve_export(
    graph: &ModuleGraph,
    index: usize,
    name: &str,
    resolve_set: &mut HashSet<(usize, String)>,
) -> Result<Option<js_runtime::value::Cell>, ModuleError> {
    if !resolve_set.insert((index, name.to_string())) {
        return Ok(None);
    }
    let module = &graph.modules[index];
    for export in &module.metadata.exports {
        match export {
            ExportEntry::Local {
                exported,
                local_slot,
            } if exported == name => return Ok(Some(module.locals[*local_slot].clone())),
            ExportEntry::Indirect {
                exported,
                request,
                imported,
            } if exported == name => {
                let dependency = module.dependencies[request];
                return resolve_export(graph, dependency, imported, resolve_set);
            }
            ExportEntry::Namespace { exported, request } if exported == name => {
                let dependency = module.dependencies[request];
                return Ok(Some(graph.modules[dependency].namespace_cell.clone()));
            }
            _ => {}
        }
    }
    if name == "default" {
        return Ok(None);
    }
    let mut star_resolution = None;
    for export in &module.metadata.exports {
        let ExportEntry::Star { request } = export else {
            continue;
        };
        let dependency = module.dependencies[request];
        if let Some(cell) = resolve_export(graph, dependency, name, resolve_set)? {
            if let Some(previous) = &star_resolution {
                if !js_runtime::value::Cell::ptr_eq(previous, &cell) {
                    return Err(ModuleError::Link {
                        module: module.key.clone(),
                        message: format!("export `{name}` is ambiguous across star exports"),
                    });
                }
            } else {
                star_resolution = Some(cell);
            }
        }
    }
    Ok(star_resolution)
}

fn module_namespace(graph: &mut ModuleGraph, index: usize) -> Result<Value, ModuleError> {
    if let Some(namespace) = &graph.modules[index].namespace {
        return Ok(namespace.clone());
    }
    let namespace = build_module_namespace(graph, index, None)?;
    graph.modules[index].namespace = Some(namespace.clone());
    Ok(namespace)
}

fn module_namespace_deferred(graph: &mut ModuleGraph, index: usize) -> Result<Value, ModuleError> {
    if let Some(namespace) = &graph.modules[index].deferred_namespace {
        return Ok(namespace.clone());
    }
    let namespace = Value::object(
        js_runtime::object::ObjectData::module_namespace_with_deferred(
            BTreeMap::new(),
            Some(index),
        ),
    );
    graph.modules[index].deferred_namespace = Some(namespace.clone());
    Ok(namespace)
}

fn build_module_namespace(
    graph: &ModuleGraph,
    index: usize,
    deferred_module: Option<usize>,
) -> Result<Value, ModuleError> {
    let mut names = HashSet::new();
    collect_export_names(graph, index, &mut HashSet::new(), &mut names);
    let mut exports = BTreeMap::new();
    for name in names {
        match resolve_export(graph, index, &name, &mut HashSet::new()) {
            Ok(Some(cell)) => {
                exports.insert(name, cell);
            }
            Ok(None) | Err(ModuleError::Link { .. }) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Value::object(
        js_runtime::object::ObjectData::module_namespace_with_deferred(exports, deferred_module),
    ))
}

fn materialize_namespaces(graph: &mut ModuleGraph) -> Result<(), EngineError> {
    for index in 0..graph.modules.len() {
        let built = build_module_namespace(graph, index, None).map_err(EngineError::Module)?;
        let exports = match built.data() {
            js_runtime::value::ValueData::Object(object) => {
                object.borrow().module_namespace.clone().unwrap_or_default()
            }
            _ => unreachable!(),
        };
        for namespace in [
            graph.modules[index].namespace.as_ref(),
            graph.modules[index].deferred_namespace.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if let js_runtime::value::ValueData::Object(object) = namespace.data() {
                object.borrow_mut().module_namespace = Some(exports.clone());
            }
        }
    }
    Ok(())
}

fn collect_export_names(
    graph: &ModuleGraph,
    index: usize,
    visited: &mut HashSet<usize>,
    names: &mut HashSet<String>,
) {
    if !visited.insert(index) {
        return;
    }
    for export in &graph.modules[index].metadata.exports {
        match export {
            ExportEntry::Local { exported, .. }
            | ExportEntry::Indirect { exported, .. }
            | ExportEntry::Namespace { exported, .. } => {
                names.insert(exported.clone());
            }
            ExportEntry::Star { request } => {
                let dependency = graph.modules[index].dependencies[request];
                let mut star_names = HashSet::new();
                collect_export_names(graph, dependency, visited, &mut star_names);
                names.extend(star_names.into_iter().filter(|name| name != "default"));
            }
        }
    }
}

struct ModuleEvaluator<'a, 'b> {
    graph: &'a mut ModuleGraph,
    bytecodes: &'b [&'b js_bytecode::BytecodeModule],
    interpreter: &'a mut js_vm::Interpreter,
    next_async_order: u64,
}

impl<'a, 'b> ModuleEvaluator<'a, 'b> {
    fn new(
        graph: &'a mut ModuleGraph,
        bytecodes: &'b [&'b js_bytecode::BytecodeModule],
        interpreter: &'a mut js_vm::Interpreter,
    ) -> Self {
        Self {
            graph,
            bytecodes,
            interpreter,
            next_async_order: 0,
        }
    }

    fn evaluate(&mut self, entry: usize) -> Result<Value, EngineError> {
        self.inner_evaluation(entry)?;
        loop {
            let mut progressed = self.process_dynamic_imports()?;
            progressed |= self.interpreter.has_promise_jobs();
            self.interpreter
                .run_promise_jobs_report(self.bytecodes, entry)
                .map_err(EngineError::from)?;
            let mut ready: Vec<_> = self
                .graph
                .modules
                .iter()
                .enumerate()
                .filter(|(index, module)| {
                    module.status == ModuleStatus::EvaluatingAsync
                        && self.interpreter.module_is_ready(*index)
                })
                .map(|(index, module)| (module.async_evaluation_order.unwrap_or(u64::MAX), index))
                .collect();
            ready.sort_unstable();
            for (_, index) in ready {
                if self.graph.modules[index].status != ModuleStatus::EvaluatingAsync
                    || !self.interpreter.module_is_ready(index)
                {
                    continue;
                }
                progressed = true;
                let outcome = self
                    .interpreter
                    .resume_module_in_graph_report(self.bytecodes, index);
                match outcome {
                    Ok(js_vm::ModuleExecution::Completed(value)) => {
                        self.complete_module(index, value)?;
                    }
                    Ok(js_vm::ModuleExecution::Suspended) => {}
                    Err(error) => self.fail_module(index, EngineError::from(error))?,
                }
            }

            if self.graph.modules[entry].status == ModuleStatus::Evaluated
                && !self.interpreter.has_dynamic_import_requests()
                && !self.interpreter.has_promise_jobs()
                && !self
                    .graph
                    .modules
                    .iter()
                    .any(|module| !module.dynamic_import_waiters.is_empty())
            {
                return Ok(self.graph.modules[entry]
                    .evaluation_value
                    .clone()
                    .unwrap_or_else(Value::undefined));
            }

            if !progressed {
                return Err(EngineError::Module(ModuleError::Unsupported {
                    module: self.graph.modules[entry].key.clone(),
                    feature: "top-level await is pending with no runnable PromiseJobs".into(),
                }));
            }
        }
    }

    fn process_dynamic_imports(&mut self) -> Result<bool, EngineError> {
        let requests = self.interpreter.take_dynamic_import_requests();
        let progressed = !requests.is_empty();
        for request in requests {
            let target = match request.resolution {
                Ok(target) => target,
                Err(message) => {
                    let reason = js_vm::builtins::error_ctor(
                        &Value::undefined(),
                        &[Value::string(message)],
                        "TypeError",
                    );
                    self.interpreter
                        .reject_host_promise(request.promise, reason);
                    continue;
                }
            };
            match self.graph.modules[target].status {
                ModuleStatus::Evaluated => {
                    let namespace = self.graph.modules[target]
                        .namespace
                        .clone()
                        .unwrap_or_else(Value::undefined);
                    self.interpreter
                        .resolve_host_promise_report(
                            self.bytecodes,
                            target,
                            request.promise,
                            namespace,
                        )
                        .map_err(EngineError::from)?;
                }
                ModuleStatus::Errored => {
                    let reason = self.graph.modules[target]
                        .evaluation_error
                        .clone()
                        .unwrap_or_else(|| {
                            js_vm::builtins::error_ctor(
                                &Value::undefined(),
                                &[Value::string("dynamic module evaluation failed")],
                                "TypeError",
                            )
                        });
                    self.interpreter
                        .reject_host_promise(request.promise, reason);
                }
                _ => {
                    self.graph.modules[target]
                        .dynamic_import_waiters
                        .push(request.promise);
                    if self.graph.modules[target].status == ModuleStatus::Linked {
                        if let Err(error) = self.inner_evaluation(target) {
                            self.fail_module(target, error)?;
                        }
                    }
                }
            }
        }
        Ok(progressed)
    }

    fn fail_module(&mut self, index: usize, error: EngineError) -> Result<(), EngineError> {
        let reason = match error {
            EngineError::Exception(exception) => exception.value,
            other => return Err(other),
        };
        self.graph.modules[index].status = ModuleStatus::Errored;
        self.graph.modules[index].evaluation_error = Some(reason.clone());
        let waiters = std::mem::take(&mut self.graph.modules[index].dynamic_import_waiters);
        if waiters.is_empty() {
            return Err(EngineError::Exception(js_vm::JsException {
                value: reason,
                source: self.graph.modules[index].compiled.source.clone().into(),
                stack: Vec::new(),
            }));
        }
        for promise in waiters {
            self.interpreter
                .reject_host_promise(promise, reason.clone());
        }
        Ok(())
    }

    fn inner_evaluation(&mut self, index: usize) -> Result<(), EngineError> {
        match self.graph.modules[index].status {
            ModuleStatus::Evaluated | ModuleStatus::Evaluating | ModuleStatus::EvaluatingAsync => {
                return Ok(())
            }
            ModuleStatus::Linked => {}
            ModuleStatus::Errored => {
                return Err(EngineError::Module(ModuleError::Link {
                    module: self.graph.modules[index].key.clone(),
                    message: "module is already in an errored state".into(),
                }))
            }
            other => {
                return Err(EngineError::Module(ModuleError::Link {
                    module: self.graph.modules[index].key.clone(),
                    message: format!("cannot evaluate module in state {other:?}"),
                }))
            }
        }
        self.graph.modules[index].status = ModuleStatus::Evaluating;

        let mut seen = HashSet::new();
        let dependency_order: Vec<_> = self.graph.modules[index]
            .metadata
            .requests
            .iter()
            .filter(|request| request.phase == js_syntax::ImportPhase::Eval)
            .filter_map(|request| {
                self.graph.modules[index]
                    .dependencies
                    .get(&request.specifier)
                    .copied()
            })
            .filter(|dependency| seen.insert(*dependency))
            .collect();
        for dependency in dependency_order {
            self.inner_evaluation(dependency)?;
            if self.graph.modules[dependency].status == ModuleStatus::EvaluatingAsync {
                self.graph.modules[index].pending_async_dependencies += 1;
                if !self.graph.modules[dependency]
                    .async_parent_modules
                    .contains(&index)
                {
                    self.graph.modules[dependency]
                        .async_parent_modules
                        .push(index);
                }
            }
        }

        if self.graph.modules[index].pending_async_dependencies == 0 {
            self.start_module(index)
        } else {
            self.mark_async(index);
            Ok(())
        }
    }

    fn start_module(&mut self, index: usize) -> Result<(), EngineError> {
        let locals = self.graph.modules[index].locals.clone();
        match self
            .interpreter
            .start_module_in_graph_report(self.bytecodes, index, locals)
            .map_err(EngineError::from)?
        {
            js_vm::ModuleExecution::Completed(value) => self.complete_module(index, value),
            js_vm::ModuleExecution::Suspended => {
                self.mark_async(index);
                Ok(())
            }
        }
    }

    fn mark_async(&mut self, index: usize) {
        self.graph.modules[index].status = ModuleStatus::EvaluatingAsync;
        if self.graph.modules[index].async_evaluation_order.is_none() {
            self.graph.modules[index].async_evaluation_order = Some(self.next_async_order);
            self.next_async_order += 1;
        }
    }

    fn complete_module(&mut self, index: usize, value: Value) -> Result<(), EngineError> {
        self.graph.modules[index].status = ModuleStatus::Evaluated;
        self.graph.modules[index].evaluation_value = Some(value);
        self.interpreter.mark_module_evaluated(index);

        let namespace = self.graph.modules[index]
            .namespace
            .clone()
            .unwrap_or_else(Value::undefined);
        let waiters = std::mem::take(&mut self.graph.modules[index].dynamic_import_waiters);
        for promise in waiters {
            self.interpreter
                .resolve_host_promise_report(self.bytecodes, index, promise, namespace.clone())
                .map_err(EngineError::from)?;
        }

        let mut available = Vec::new();
        self.gather_available_ancestors(index, &mut available);
        available.sort_by_key(|parent| {
            self.graph.modules[*parent]
                .async_evaluation_order
                .unwrap_or(u64::MAX)
        });
        for parent in available {
            if self.graph.modules[parent].status == ModuleStatus::EvaluatingAsync
                && self.graph.modules[parent].pending_async_dependencies == 0
            {
                self.start_module(parent)?;
            }
        }
        Ok(())
    }

    fn gather_available_ancestors(&mut self, index: usize, available: &mut Vec<usize>) {
        let parents = std::mem::take(&mut self.graph.modules[index].async_parent_modules);
        for parent in parents {
            let pending = &mut self.graph.modules[parent].pending_async_dependencies;
            *pending = pending.saturating_sub(1);
            if *pending == 0
                && self.graph.modules[parent].status == ModuleStatus::EvaluatingAsync
                && !available.contains(&parent)
            {
                available.push(parent);
                self.gather_available_ancestors(parent, available);
            }
        }
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
