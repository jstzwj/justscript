//! The compiled-module abstraction: a single type holding whatever a given
//! [`crate::ExecutionMode`] produced.

use js_bytecode::BytecodeModule;
use js_runtime::value::{Cell, Value};
use js_syntax::ast::pat::{ArrayPatElement, ObjectPatProp, Pat};
use js_syntax::ast::stmt::{Decl, ExportSpec, ImportSpec, ModuleRequest, VarKind};
use js_syntax::{ImportPhase, Program, ProgramItem, SourceFile};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// The artifact produced by compiling a source program.
///
/// - Always carries the parsed AST and the bytecode (the universal IR).
/// - Carries native code only when JIT/AOT ran successfully.
pub struct CompiledModule {
    pub source: Arc<SourceFile>,
    pub program: Program,
    pub bytecode: BytecodeModule,
    #[cfg(feature = "jit")]
    pub native: Option<crate::pipeline::NativeArtifact>,
}

impl CompiledModule {
    pub fn new(
        source: Arc<SourceFile>,
        program: Program,
        bytecode: BytecodeModule,
    ) -> CompiledModule {
        CompiledModule {
            source,
            program,
            bytecode,
            #[cfg(feature = "jit")]
            native: None,
        }
    }
}

/// Host boundary for resolving and loading ECMAScript modules.
pub trait ModuleLoader {
    /// Resolve `specifier` relative to the canonical `referrer` key. The entry
    /// module is resolved with `referrer == None`.
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String>;

    /// Load the source text identified by a canonical key returned by
    /// [`Self::resolve`].
    fn load(&self, key: &str) -> Result<Arc<str>, String>;
}

/// Deterministic in-memory host used by embedders and module unit tests.
#[derive(Default)]
pub struct MemoryModuleLoader {
    modules: HashMap<String, Arc<str>>,
}

impl MemoryModuleLoader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, key: impl Into<String>, source: impl Into<Arc<str>>) {
        self.modules
            .insert(normalize_key(&key.into()), source.into());
    }
}

impl ModuleLoader for MemoryModuleLoader {
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String> {
        let key = if specifier.starts_with("./") || specifier.starts_with("../") {
            let base = referrer
                .and_then(|name| Path::new(name).parent())
                .unwrap_or_else(|| Path::new(""));
            normalize_path(base.join(specifier))
        } else {
            normalize_key(specifier)
        };
        self.modules
            .contains_key(&key)
            .then_some(key.clone())
            .ok_or_else(|| format!("module `{key}` is not registered"))
    }

    fn load(&self, key: &str) -> Result<Arc<str>, String> {
        self.modules
            .get(key)
            .cloned()
            .ok_or_else(|| format!("module `{key}` is not registered"))
    }
}

/// Filesystem-backed host. Resolution canonicalizes paths, making graph cache
/// identity independent of `.` and `..` spelling.
#[derive(Default)]
pub struct FileModuleLoader;

impl ModuleLoader for FileModuleLoader {
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String> {
        let path = if let Some(referrer) = referrer {
            Path::new(referrer)
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(specifier)
        } else {
            PathBuf::from(specifier)
        };
        std::fs::canonicalize(&path)
            .map(|path| path.display().to_string())
            .map_err(|error| format!("cannot resolve `{}`: {error}", path.display()))
    }

    fn load(&self, key: &str) -> Result<Arc<str>, String> {
        std::fs::read_to_string(key)
            .map(Arc::<str>::from)
            .map_err(|error| format!("cannot load `{key}`: {error}"))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ModuleStatus {
    Unlinked,
    Linking,
    Linked,
    Evaluating,
    /// Evaluation crossed a top-level `await` and is running PromiseJobs.
    EvaluatingAsync,
    Evaluated,
    Errored,
}

#[derive(Clone, Debug)]
pub enum ModuleError {
    Resolve {
        referrer: Option<String>,
        specifier: String,
        message: String,
    },
    Load {
        module: String,
        message: String,
    },
    Link {
        module: String,
        message: String,
    },
    Unsupported {
        module: String,
        feature: String,
    },
}

impl fmt::Display for ModuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModuleError::Resolve {
                referrer,
                specifier,
                message,
            } => write!(
                f,
                "cannot resolve `{specifier}` from `{}`: {message}",
                referrer.as_deref().unwrap_or("<entry>")
            ),
            ModuleError::Load { module, message } => {
                write!(f, "cannot load module `{module}`: {message}")
            }
            ModuleError::Link { module, message } => {
                write!(f, "cannot link module `{module}`: {message}")
            }
            ModuleError::Unsupported { module, feature } => {
                write!(
                    f,
                    "module `{module}` uses unsupported runtime feature: {feature}"
                )
            }
        }
    }
}

impl std::error::Error for ModuleError {}

#[derive(Clone)]
pub(crate) struct RequestEntry {
    pub specifier: String,
    pub phase: ImportPhase,
}

#[derive(Clone)]
pub(crate) enum ImportedName {
    Name(String),
    Namespace,
}

#[derive(Clone)]
pub(crate) struct ImportEntry {
    pub request: String,
    pub phase: ImportPhase,
    pub imported: ImportedName,
    pub local_slot: usize,
}

#[derive(Clone)]
pub(crate) enum ExportEntry {
    Local {
        exported: String,
        local_slot: usize,
    },
    Indirect {
        exported: String,
        request: String,
        imported: String,
    },
    Star {
        request: String,
    },
    Namespace {
        exported: String,
        request: String,
    },
}

#[derive(Default)]
pub(crate) struct ModuleMetadata {
    pub requests: Vec<RequestEntry>,
    pub imports: Vec<ImportEntry>,
    pub exports: Vec<ExportEntry>,
    /// Lexical slots start in the temporal dead zone until their declaration's
    /// bytecode performs the first store.
    pub uninitialized_slots: HashSet<usize>,
    pub immutable_slots: HashSet<usize>,
    pub has_top_level_await: bool,
}

pub(crate) struct RuntimeModule {
    pub key: String,
    pub compiled: Rc<CompiledModule>,
    pub metadata: ModuleMetadata,
    pub dependencies: HashMap<String, usize>,
    pub locals: Vec<Cell>,
    pub namespace: Option<Value>,
    pub namespace_cell: Cell,
    pub deferred_namespace: Option<Value>,
    pub status: ModuleStatus,
}

#[derive(Default)]
pub(crate) struct ModuleGraph {
    pub modules: Vec<RuntimeModule>,
    pub by_key: HashMap<String, usize>,
}

pub(crate) fn analyze_module(compiled: &CompiledModule) -> Result<ModuleMetadata, ModuleError> {
    let mut metadata = ModuleMetadata::default();
    metadata.has_top_level_await = compiled
        .bytecode
        .main
        .code
        .iter()
        .any(|instruction| instruction.op == js_bytecode::Opcode::Await);
    for item in &compiled.program.body {
        let ProgramItem::Decl(decl) = item else {
            continue;
        };
        match decl {
            Decl::Import { spec, .. } => analyze_import(
                spec,
                &compiled.bytecode,
                &compiled.source.name,
                &mut metadata,
            )?,
            Decl::Export { spec, .. } => analyze_export(
                spec,
                &compiled.bytecode,
                &compiled.source.name,
                &mut metadata,
            )?,
            other => analyze_lexical_bindings(other, &compiled.bytecode, &mut metadata),
        }
    }
    Ok(metadata)
}

fn analyze_import(
    spec: &ImportSpec,
    bytecode: &BytecodeModule,
    module: &str,
    metadata: &mut ModuleMetadata,
) -> Result<(), ModuleError> {
    let request = match spec {
        ImportSpec::Bare { request }
        | ImportSpec::Namespace { request, .. }
        | ImportSpec::Named { request, .. }
        | ImportSpec::Default { request, .. } => request,
    };
    push_request(metadata, request);
    match spec {
        ImportSpec::Bare { .. } => {}
        ImportSpec::Namespace { ns, .. } => metadata.imports.push(ImportEntry {
            request: request.specifier.clone(),
            phase: request.phase,
            imported: ImportedName::Namespace,
            local_slot: local_slot(bytecode, module, ns)?,
        }),
        ImportSpec::Named { items, .. } => {
            for item in items {
                metadata.imports.push(ImportEntry {
                    request: request.specifier.clone(),
                    phase: request.phase,
                    imported: ImportedName::Name(item.imported.value().to_string()),
                    local_slot: local_slot(bytecode, module, &item.local)?,
                });
            }
        }
        ImportSpec::Default {
            local,
            namespace,
            named,
            ..
        } => {
            metadata.imports.push(ImportEntry {
                request: request.specifier.clone(),
                phase: request.phase,
                imported: ImportedName::Name("default".into()),
                local_slot: local_slot(bytecode, module, local)?,
            });
            if let Some(namespace) = namespace {
                metadata.imports.push(ImportEntry {
                    request: request.specifier.clone(),
                    phase: request.phase,
                    imported: ImportedName::Namespace,
                    local_slot: local_slot(bytecode, module, namespace)?,
                });
            }
            for item in named {
                metadata.imports.push(ImportEntry {
                    request: request.specifier.clone(),
                    phase: request.phase,
                    imported: ImportedName::Name(item.imported.value().to_string()),
                    local_slot: local_slot(bytecode, module, &item.local)?,
                });
            }
        }
    }
    Ok(())
}

fn analyze_export(
    spec: &ExportSpec,
    bytecode: &BytecodeModule,
    module: &str,
    metadata: &mut ModuleMetadata,
) -> Result<(), ModuleError> {
    match spec {
        ExportSpec::Named { items } => {
            for item in items {
                metadata.exports.push(ExportEntry::Local {
                    exported: item.exported.value().to_string(),
                    local_slot: local_slot(bytecode, module, item.local.value())?,
                });
            }
        }
        ExportSpec::Default(_) => {
            let slot = local_slot(bytecode, module, js_bytecode::DEFAULT_EXPORT_LOCAL)?;
            metadata.exports.push(ExportEntry::Local {
                exported: "default".into(),
                local_slot: slot,
            });
            metadata.uninitialized_slots.insert(slot);
        }
        ExportSpec::DefaultDecl(decl) => {
            let local = declaration_names(decl)
                .into_iter()
                .next()
                .unwrap_or_else(|| js_bytecode::DEFAULT_EXPORT_LOCAL.to_string());
            metadata.exports.push(ExportEntry::Local {
                exported: "default".into(),
                local_slot: local_slot(bytecode, module, &local)?,
            });
            analyze_lexical_bindings(decl, bytecode, metadata);
        }
        ExportSpec::Decl(decl) => {
            for name in declaration_names(decl) {
                metadata.exports.push(ExportEntry::Local {
                    exported: name.clone(),
                    local_slot: local_slot(bytecode, module, &name)?,
                });
            }
            analyze_lexical_bindings(decl, bytecode, metadata);
        }
        ExportSpec::ReExport { items, request } => {
            push_request(metadata, request);
            for item in items {
                metadata.exports.push(ExportEntry::Indirect {
                    exported: item.exported.value().to_string(),
                    request: request.specifier.clone(),
                    imported: item.local.value().to_string(),
                });
            }
        }
        ExportSpec::All { exported, request } => {
            push_request(metadata, request);
            if let Some(exported) = exported {
                metadata.exports.push(ExportEntry::Namespace {
                    exported: exported.value().to_string(),
                    request: request.specifier.clone(),
                });
            } else {
                metadata.exports.push(ExportEntry::Star {
                    request: request.specifier.clone(),
                });
            }
        }
    }
    Ok(())
}

fn analyze_lexical_bindings(decl: &Decl, bytecode: &BytecodeModule, metadata: &mut ModuleMetadata) {
    let is_lexical = match decl {
        Decl::Var { kind, .. } => !matches!(kind, VarKind::Var),
        Decl::Class(_) => true,
        _ => false,
    };
    if is_lexical {
        for name in declaration_names(decl) {
            if let Some(slot) = bytecode.main.locals.get(&name) {
                metadata.uninitialized_slots.insert(usize::from(slot));
                if matches!(
                    decl,
                    Decl::Var {
                        kind: VarKind::Const | VarKind::Using | VarKind::AwaitUsing,
                        ..
                    }
                ) {
                    metadata.immutable_slots.insert(usize::from(slot));
                }
            }
        }
    }
}

fn push_request(metadata: &mut ModuleMetadata, request: &ModuleRequest) {
    if !metadata
        .requests
        .iter()
        .any(|entry| entry.specifier == request.specifier && entry.phase == request.phase)
    {
        metadata.requests.push(RequestEntry {
            specifier: request.specifier.clone(),
            phase: request.phase,
        });
    }
}

fn local_slot(bytecode: &BytecodeModule, module: &str, name: &str) -> Result<usize, ModuleError> {
    bytecode
        .main
        .locals
        .get(name)
        .map(usize::from)
        .ok_or_else(|| ModuleError::Link {
            module: module.to_string(),
            message: format!("module binding `{name}` has no bytecode local slot"),
        })
}

fn declaration_names(decl: &Decl) -> Vec<String> {
    match decl {
        Decl::Var { declarations, .. } => declarations
            .iter()
            .flat_map(|declaration| pattern_names(&declaration.name))
            .collect(),
        Decl::Function(function) => function.name.iter().cloned().collect(),
        Decl::Class(class) => class.name.iter().cloned().collect(),
        Decl::Export {
            spec: ExportSpec::Decl(decl) | ExportSpec::DefaultDecl(decl),
            ..
        } => declaration_names(decl),
        _ => Vec::new(),
    }
}

fn pattern_names(pattern: &Pat) -> Vec<String> {
    let mut names = Vec::new();
    collect_pattern_names(pattern, &mut names);
    names
}

fn collect_pattern_names(pattern: &Pat, names: &mut Vec<String>) {
    match pattern {
        Pat::Ident { name, .. } => names.push(name.clone()),
        Pat::Array { elements, .. } => {
            for element in elements.iter().flatten() {
                if let ArrayPatElement::Pat(pattern) = element {
                    collect_pattern_names(pattern, names);
                }
            }
        }
        Pat::Object { properties, .. } => {
            for property in properties {
                match property {
                    ObjectPatProp::KeyValue { value, .. } => collect_pattern_names(value, names),
                    ObjectPatProp::Rest { arg, .. } => collect_pattern_names(arg, names),
                }
            }
        }
        Pat::Rest { arg, .. } => collect_pattern_names(arg, names),
        Pat::Assignment { left, .. } => collect_pattern_names(left, names),
        Pat::Member(_) => {}
    }
}

pub(crate) fn fresh_module_cells(
    bytecode: &BytecodeModule,
    metadata: &ModuleMetadata,
) -> Vec<Cell> {
    (0..bytecode.main.locals.slot_count())
        .map(|slot| {
            if metadata.uninitialized_slots.contains(&usize::from(slot)) {
                Cell::uninitialized(!metadata.immutable_slots.contains(&usize::from(slot)))
            } else {
                Cell::mutable(Value::undefined())
            }
        })
        .collect()
}

fn normalize_key(key: &str) -> String {
    normalize_path(PathBuf::from(key))
}

fn normalize_path(path: PathBuf) -> String {
    let absolute = path.is_absolute();
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => parts.push(prefix.as_os_str().to_owned()),
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_owned()),
        }
    }
    let mut normalized = PathBuf::new();
    if absolute {
        normalized.push(Path::new("/"));
    }
    for part in parts {
        normalized.push(part);
    }
    normalized.display().to_string()
}
