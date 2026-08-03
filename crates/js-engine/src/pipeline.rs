//! The end-to-end pipeline: source → AST → bytecode → (interpret | JIT | AOT).

use crate::config::{EngineConfig, ExecutionMode};
use crate::module::{
    analyze_module, fresh_module_environment, synthetic_text_module_source, CompiledModule,
    DynamicResolution, ExportEntry, ImportedName, ModuleError, ModuleGraph, ModuleIdentity,
    ModuleLoader, ModuleStatus, ModuleType, RequestEntry, RuntimeModule,
    SYNTHETIC_JSON_MODULE_SOURCE,
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
        let entry_identity = ModuleIdentity::new(entry_key, ModuleType::JavaScript);
        // Create the interpreter before loading the graph: its `Interpreter::new`
        // installs the realm globals and per-realm prototypes on the *shared*
        // realm (`RealmContext.realm` is `Rc<RefCell<Realm>>`), so synthetic
        // modules parsed at load time (JSON) can link their values to the
        // realm's own Array/Object prototypes. The interpreter is configured and
        // driven later, once the graph is linked.
        let mut interpreter = js_vm::Interpreter::new(self.ctx_realm_clone());
        let mut graph = ModuleGraph::default();
        let entry_index = self.load_module_graph(loader, &entry_identity, &mut graph)?;
        link_module(&mut graph, entry_index).map_err(EngineError::Module)?;
        for index in 0..graph.modules.len() {
            if graph.modules[index].status == ModuleStatus::Unlinked {
                link_module(&mut graph, index).map_err(EngineError::Module)?;
            }
        }
        // Import bindings must exist before hoisted functions close over the
        // module environment. This is ModuleDeclarationInstantiation order,
        // and matters for functions that reference an imported binding.
        instantiate_module_functions(&mut graph)?;
        materialize_namespaces(&mut graph)?;

        // Keep bytecode owners separate from mutable graph state while the VM
        // dispatches functions across module boundaries.
        let owners: Vec<Rc<CompiledModule>> = graph
            .modules
            .iter()
            .map(|module| module.compiled.clone())
            .collect();
        let bytecodes: Vec<_> = owners.iter().map(|module| &module.bytecode).collect();
        let module_locals = graph
            .modules
            .iter()
            .map(|module| module.environment.snapshot())
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
                    .filter_map(|request| module.dependencies.get(request).copied())
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
                    .map(|resolution| match resolution {
                        DynamicResolution::Resolved(index) => Ok(*index),
                        DynamicResolution::Unresolved(message) => Err(message.clone()),
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
        identity: &ModuleIdentity,
        graph: &mut ModuleGraph,
    ) -> Result<usize, EngineError> {
        if let Some(index) = graph.by_key.get(identity) {
            return Ok(*index);
        }
        let key = identity.canonical_url.as_str();
        let source_text = loader.load(key).map_err(|message| {
            EngineError::Module(ModuleError::Load {
                module: key.to_string(),
                message,
            })
        })?;
        // Select and validate the module type up-front (C2). An unsupported
        // `type` attribute becomes a structured link error rather than letting
        // the wrong parser goal run on the source.
        let mut default_export_value: Option<Value> = None;
        let module_text: String = match identity.module_type {
            ModuleType::JavaScript => source_text.to_string(),
            ModuleType::Json => {
                // `ParseJSONModule`: the default export is the value of parsing
                // the source with the engine's intrinsic JSON parser at load
                // time — never a realm-global `JSON.parse` lookup a dependency
                // could have mutated before this module evaluates. Invalid JSON
                // is a link error. The realm's own prototypes are linked onto
                // arrays/objects in the result. The interpreter created in
                // `run_module` has already installed the per-realm prototypes on
                // the shared realm, so read them straight off `ctx.realm`.
                let (array_proto, object_proto) = {
                    let realm = self.ctx.realm.borrow();
                    (
                        realm.array_proto.as_ref().map(|h| Value::object(h.clone())),
                        realm
                            .object_proto
                            .as_ref()
                            .map(|h| Value::object(h.clone())),
                    )
                };
                let value =
                    js_vm::builtins::parse_json_intrinsic(&source_text, array_proto, object_proto)
                        .map_err(|message| {
                            EngineError::Module(ModuleError::Load {
                                module: key.to_string(),
                                message: format!("invalid JSON module source: {message}"),
                            })
                        })?;
                default_export_value = Some(value);
                SYNTHETIC_JSON_MODULE_SOURCE.to_string()
            }
            ModuleType::Text => synthetic_text_module_source(&source_text),
        };
        let compiled = Rc::new(
            self.compile_named(key, &module_text, ProgramKind::Module)
                .map_err(EngineError::Compile)?,
        );
        let metadata = analyze_module(&compiled).map_err(EngineError::Module)?;
        let requests = metadata.requests.clone();
        let dynamic_requests = metadata.dynamic_requests.clone();
        let index = graph.modules.len();
        graph.by_key.insert(identity.clone(), index);
        let namespace = Value::object(js_runtime::object::ObjectData::module_namespace(
            BTreeMap::new(),
        ));
        let mut environment = fresh_module_environment(&compiled.bytecode, &metadata);
        // Inject a synthetic JSON module's pre-parsed default export into the
        // `*default*` cell before the record is published, so import bindings
        // captured during instantiation resolve to this value. The module's own
        // bytecode is never evaluated (see `start_module`).
        if let Some(value) = &default_export_value {
            if let Some(slot) = compiled
                .bytecode
                .main
                .locals
                .get(js_bytecode::DEFAULT_EXPORT_LOCAL)
            {
                environment.set_local(
                    usize::from(slot),
                    js_runtime::value::Cell::initialized(value.clone(), false),
                );
            }
        }
        graph.modules.push(RuntimeModule {
            key: key.to_string(),
            environment,
            compiled,
            metadata,
            dependencies: Default::default(),
            dynamic_dependencies: Default::default(),
            namespace: Some(namespace.clone()),
            namespace_cell: js_runtime::value::Cell::initialized(namespace, false),
            // Every Module Record owns exactly one stable ModuleSource cell.
            // Source-phase imports bind directly to this cell, and two source
            // imports resolving to the same Module Record therefore share
            // cell/value identity (C4).
            module_source_cell: js_runtime::value::Cell::initialized(
                js_vm::builtins::new_module_source(),
                false,
            ),
            deferred_namespace: None,
            status: ModuleStatus::Unlinked,
            pending_async_dependencies: 0,
            async_parent_modules: Vec::new(),
            async_evaluation_order: None,
            evaluation_value: None,
            evaluation_error: None,
            default_export_value,
            dynamic_import_waiters: Vec::new(),
            module_type: identity.module_type,
        });

        for request in requests {
            // Validate the importer's attribute selection (C2) before resolving
            // the dependency. Unsupported types surface as link errors.
            let dep_module_type = request.resolve_module_type().map_err(|message| {
                EngineError::Module(ModuleError::Link {
                    module: key.to_string(),
                    message,
                })
            })?;
            let resolved = loader
                .resolve(Some(key), &request.specifier)
                .map_err(|message| {
                    EngineError::Module(ModuleError::Resolve {
                        referrer: Some(key.to_string()),
                        specifier: request.specifier.clone(),
                        message,
                    })
                })?;
            let dep_identity = ModuleIdentity::new(resolved, dep_module_type);
            let dependency = self.load_module_graph(loader, &dep_identity, graph)?;
            graph.modules[index]
                .dependencies
                .insert(request, dependency);
        }
        for request in dynamic_requests {
            // Each dynamic-import `ModuleRequest` (specifier + phase + literal
            // attributes from `import(src, { with: { ... } })`) is preloaded as
            // the correct TYPED record (JSON/text/JS). Resolutions are pushed in
            // source order so the per-request index carried by the `DynamicImport`
            // opcode aligns with this Vec — keeping two imports of the same
            // specifier with different attributes distinct, and sharing a record
            // with any static import of the same canonical URL + type.
            let entry = RequestEntry {
                specifier: request.specifier.clone(),
                phase: request.phase,
                attributes: request.attributes.clone(),
            };
            let module_type = match entry.resolve_module_type() {
                Ok(module_type) => module_type,
                Err(message) => {
                    graph.modules[index]
                        .dynamic_dependencies
                        .push(DynamicResolution::Unresolved(message));
                    continue;
                }
            };
            let resolution = match loader.resolve(Some(key), &request.specifier) {
                Ok(resolved) => {
                    let dyn_identity = ModuleIdentity::new(resolved, module_type);
                    match self.load_module_graph(loader, &dyn_identity, graph) {
                        Ok(dependency) => DynamicResolution::Resolved(dependency),
                        Err(error) => DynamicResolution::Unresolved(error.to_string()),
                    }
                }
                Err(message) => DynamicResolution::Unresolved(message),
            };
            graph.modules[index].dynamic_dependencies.push(resolution);
        }
        Ok(index)
    }

    /// Install the test262 harness globals (`assert`, `Test262Error`, `$DONE`)
    /// into this engine's realm. Persist for the life of the engine (the realm
    /// is shared across `execute` calls). Idempotent.
    pub fn install_test262_harness(&mut self) {
        let mut realm = self.ctx.realm.borrow_mut();
        realm.test262_done_called = false;
        realm.test262_done_error = None;
        js_vm::builtins::install_test262_harness(&mut realm.globals);
    }

    /// Whether `$DONE` was observed in this engine's dedicated Test262 realm.
    pub fn test262_done_called(&self) -> bool {
        self.ctx.realm.borrow().test262_done_called
    }

    /// The failure value passed to `$DONE`, if any. When present, the test must
    /// be classified as failed regardless of the top-level completion value,
    /// because the throw is swallowed by the surrounding Promise reaction.
    pub fn test262_done_error(&self) -> Option<Value> {
        self.ctx.realm.borrow().test262_done_error.clone()
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
                module.environment.cells(),
            )
            .map_err(|message| {
                EngineError::Module(ModuleError::Link {
                    module: module.key.clone(),
                    message,
                })
            })?;
            module
                .environment
                .binding(usize::from(slot))
                .set(value)
                .map_err(|_| {
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
        .filter_map(|request| graph.modules[index].dependencies.get(request).copied())
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
                message: format!("request {} was not loaded", import.request.describe()),
            })?;
        let cell = match import.imported {
            ImportedName::Name(name) => {
                resolve_export(graph, dependency, &name, &mut HashSet::new())?.ok_or_else(|| {
                    ModuleError::Link {
                        module: graph.modules[index].key.clone(),
                        message: format!(
                            "module `{}` ({:?}) does not export `{name}`",
                            graph.modules[dependency].key, graph.modules[dependency].module_type
                        ),
                    }
                })?
            }
            // Source-phase import (C4): bind the local immutable import slot
            // directly to the TARGET module's `module_source_cell`. Two source
            // imports resolving to the same Module Record therefore observe
            // cell identity (required for unambiguous star re-export).
            ImportedName::Source => graph.modules[dependency].module_source_cell.clone(),
            ImportedName::Namespace => {
                if import.phase == js_syntax::ImportPhase::Defer {
                    let namespace = module_namespace_deferred(graph, dependency)?;
                    js_runtime::value::Cell::initialized(namespace, false)
                } else {
                    let _ = module_namespace(graph, dependency)?;
                    graph.modules[dependency].namespace_cell.clone()
                }
            }
        };
        bindings.push((import.local_slot, cell));
    }
    for (slot, cell) in bindings {
        graph.modules[index]
            .environment
            .create_import_binding(slot, cell);
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

/// If `cell` is (or indirectly points at) some runtime module's
/// `module_source_cell`, return that source cell. This is the cell-identity
/// test that lets a source-phase re-export (`import source x; export { x };`)
/// resolve to the underlying ModuleSource. [`Cell::ptr_eq`] already recurses
/// through indirect import cells, so a local binding created via
/// `create_import_binding(slot, target.module_source_cell)` is recognised here.
fn find_module_source_cell(
    graph: &ModuleGraph,
    cell: &js_runtime::value::Cell,
) -> Option<js_runtime::value::Cell> {
    for module in &graph.modules {
        if js_runtime::value::Cell::ptr_eq(cell, &module.module_source_cell) {
            return Some(module.module_source_cell.clone());
        }
    }
    None
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
            } if exported == name => {
                let local_cell = module.environment.binding(*local_slot);
                // Source-phase re-export (C4): `import source x from "...";
                // export { x };` reclassifies into an indirect ExportEntry
                // whose [[BindingName]] is ~source~. We detect it by cell
                // identity — the local binding was created as an immutable
                // import of the target's `module_source_cell`. Resolving the
                // re-export therefore returns that shared source cell, which is
                // what makes `ns.x` and named re-imports observe the
                // ModuleSource and what lets two star exports of the same
                // source binding agree (unambiguous).
                if let Some(source_cell) = find_module_source_cell(graph, &local_cell) {
                    return Ok(Some(source_cell));
                }
                return Ok(Some(local_cell));
            }
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
            .filter_map(|request| self.graph.modules[index].dependencies.get(request).copied())
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
        // Synthetic JSON modules never execute bytecode: their default export is
        // the host-parsed JSON value (`ParseJSONModule`), pre-set into the
        // `*default*` cell at load time. Running the skeleton's
        // `export default null;` would clobber it, and would also re-introduce a
        // realm-global dependency if the skeleton ever changed — so skip
        // evaluation entirely. The module completes immediately; importers and
        // the namespace read the pre-set cell.
        if self.graph.modules[index].module_type == ModuleType::Json
            && self.graph.modules[index].default_export_value.is_some()
        {
            return self.complete_module(index, Value::undefined());
        }
        let locals = self.graph.modules[index].environment.snapshot();
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
