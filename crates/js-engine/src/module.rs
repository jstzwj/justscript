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

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct RequestEntry {
    pub specifier: String,
    pub phase: ImportPhase,
    /// Canonicalized import attributes; source order is not part of request
    /// identity.
    pub attributes: Vec<(String, String)>,
}

impl RequestEntry {
    fn from_request(request: &ModuleRequest) -> Self {
        let mut attributes: Vec<_> = request
            .attributes
            .iter()
            .map(|attribute| (attribute.key.clone(), attribute.value.clone()))
            .collect();
        attributes.sort();
        Self {
            specifier: request.specifier.clone(),
            phase: request.phase,
            attributes,
        }
    }

    pub fn module_type(&self) -> Option<&str> {
        self.attributes
            .iter()
            .find_map(|(key, value)| (key == "type").then_some(value.as_str()))
    }

    /// Render the request for diagnostics: the specifier plus its attributes
    /// explicitly, so phase/attribute identity is never silently dropped. We do
    /// NOT implement `Display` because that would invite misleading prints that
    /// elide the very fields that distinguish requests.
    pub fn describe(&self) -> String {
        if self.attributes.is_empty() {
            format!("`{}` (no attributes)", self.specifier)
        } else {
            let attrs: Vec<String> = self
                .attributes
                .iter()
                .map(|(key, value)| format!("{key}: {value:?}"))
                .collect();
            format!("`{}` with {{ {} }}", self.specifier, attrs.join(", "))
        }
    }
}

/// The module type derived from normalized import attributes.
///
/// Per `ParseJSONModule` / `CreateTextModule`, only the `type` attribute
/// selects a non-JavaScript module record; everything else is plain JavaScript.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ModuleType {
    JavaScript,
    Json,
    Text,
}

impl ModuleType {
    /// Select the module type from a normalized `type` attribute value.
    ///
    /// - no `type` attribute → [`ModuleType::JavaScript`];
    /// - `type: "json"` → [`ModuleType::Json`];
    /// - `type: "text"` → [`ModuleType::Text`];
    /// - any other value → an `Err` carrying the structured diagnostic. The
    ///   caller surfaces this as a module link error (a SyntaxError-like
    ///   outcome), never an internal VM fault.
    fn from_type_attribute(value: Option<&str>) -> Result<ModuleType, String> {
        match value {
            None => Ok(ModuleType::JavaScript),
            Some("json") => Ok(ModuleType::Json),
            Some("text") => Ok(ModuleType::Text),
            Some(other) => Err(format!("unsupported import attribute `type: {other:?}`")),
        }
    }
}

impl RequestEntry {
    /// Resolve this request's module type from its normalized attributes.
    pub(crate) fn resolve_module_type(&self) -> Result<ModuleType, String> {
        ModuleType::from_type_attribute(self.module_type())
    }
}

/// Graph/cache identity for a loaded Module Record.
///
/// Identity is the canonical URL **plus** the derived [`ModuleType`], so the
/// same canonical URL requested once as JavaScript and once as
/// `{ type: "text" }` produces two distinct Module Records (e.g. the
/// `text-self.js` entry loaded as JavaScript vs the same file self-imported as
/// text). This mirrors `FinishLoadingImportedModule`'s requirement that the
/// cache key carry the full ModuleRequest identity.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ModuleIdentity {
    pub canonical_url: String,
    pub module_type: ModuleType,
}

impl ModuleIdentity {
    pub fn new(canonical_url: impl Into<String>, module_type: ModuleType) -> Self {
        Self {
            canonical_url: canonical_url.into(),
            module_type,
        }
    }
}

#[derive(Clone)]
pub(crate) enum ImportedName {
    Name(String),
    Namespace,
    Source,
}

#[derive(Clone)]
pub(crate) struct ImportEntry {
    pub request: RequestEntry,
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
        request: RequestEntry,
        imported: String,
    },
    Star {
        request: RequestEntry,
    },
    Namespace {
        exported: String,
        request: RequestEntry,
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
    pub dynamic_requests: Vec<js_bytecode::DynamicModuleRequest>,
}

#[derive(Clone)]
pub(crate) enum DynamicResolution {
    Resolved(usize),
    Unresolved(String),
}

pub(crate) struct RuntimeModule {
    pub key: String,
    pub compiled: Rc<CompiledModule>,
    pub metadata: ModuleMetadata,
    pub dependencies: HashMap<RequestEntry, usize>,
    /// Dynamic-import resolutions, indexed by the request index stored in the
    /// `DynamicImport` opcode (which aligns 1:1 with `metadata.dynamic_requests`
    /// in source order). Indexing by request — not specifier — keeps two imports
    /// of the same specifier with different attributes distinct.
    pub dynamic_dependencies: Vec<DynamicResolution>,
    pub environment: ModuleEnvironment,
    pub namespace: Option<Value>,
    pub namespace_cell: Cell,
    /// Per-Module-Record identity exposed by source-phase imports.
    pub module_source_cell: Cell,
    pub deferred_namespace: Option<Value>,
    pub status: ModuleStatus,
    pub pending_async_dependencies: usize,
    pub async_parent_modules: Vec<usize>,
    pub async_evaluation_order: Option<u64>,
    pub evaluation_value: Option<Value>,
    pub evaluation_error: Option<Value>,
    /// Pre-parsed default export for synthetic JSON modules (`ParseJSONModule`):
    /// the value produced by the engine's intrinsic JSON parser at load time,
    /// injected into the `*default*` cell without evaluating any bytecode. This
    /// makes the module's default export independent of a realm-global
    /// `JSON.parse` a dependency could have mutated. `None` for ordinary and
    /// text modules (which evaluate their own `export default`).
    pub default_export_value: Option<Value>,
    pub dynamic_import_waiters: Vec<js_runtime::object::JsObject>,
    /// The type selected for this Module Record from its importer's normalized
    /// attributes (or [`ModuleType::JavaScript`] for the entry). Synthetic
    /// JSON/text records carry [`ModuleType::Json`] / [`ModuleType::Text`] so
    /// the linker, evaluator, and namespace builder can specialize them
    /// without re-deriving from attributes.
    pub module_type: ModuleType,
}

/// Slot-backed Module Environment Record.
///
/// Bytecode addresses bindings by slot, while linking operates on live cells.
/// This type is the boundary between those representations: direct bindings
/// are allocated when the module record is created and import bindings replace
/// their reserved slot with an immutable indirect cell during instantiation.
#[derive(Clone)]
pub(crate) struct ModuleEnvironment {
    bindings: Vec<Cell>,
}

impl ModuleEnvironment {
    pub fn binding(&self, slot: usize) -> Cell {
        self.bindings[slot].clone()
    }

    pub fn cells(&self) -> &[Cell] {
        &self.bindings
    }

    pub fn snapshot(&self) -> Vec<Cell> {
        self.bindings.clone()
    }

    pub fn create_import_binding(&mut self, slot: usize, target: Cell) {
        self.bindings[slot] = Cell::immutable_import(target);
    }

    /// Replace a direct local's cell with `cell`. Used to inject a host-prepared
    /// value (e.g. a synthetic JSON module's default export) before instantiation
    /// links import bindings, so importers capture the injected cell.
    pub fn set_local(&mut self, slot: usize, cell: Cell) {
        self.bindings[slot] = cell;
    }
}

#[derive(Default)]
pub(crate) struct ModuleGraph {
    pub modules: Vec<RuntimeModule>,
    /// Graph cache keyed by full [`ModuleIdentity`] (canonical URL + module
    /// type). The same URL loaded as both JavaScript and
    /// `{ type: "text" }` therefore occupies two distinct slots.
    pub by_key: HashMap<ModuleIdentity, usize>,
}

pub(crate) fn analyze_module(compiled: &CompiledModule) -> Result<ModuleMetadata, ModuleError> {
    let mut metadata = ModuleMetadata::default();
    metadata.dynamic_requests = compiled.bytecode.dynamic_import_requests.clone();
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
            request: RequestEntry::from_request(request),
            phase: request.phase,
            imported: ImportedName::Namespace,
            local_slot: local_slot(bytecode, module, ns)?,
        }),
        ImportSpec::Named { items, .. } => {
            for item in items {
                metadata.imports.push(ImportEntry {
                    request: RequestEntry::from_request(request),
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
                request: RequestEntry::from_request(request),
                phase: request.phase,
                imported: if request.phase == ImportPhase::Source {
                    ImportedName::Source
                } else {
                    ImportedName::Name("default".into())
                },
                local_slot: local_slot(bytecode, module, local)?,
            });
            if let Some(namespace) = namespace {
                metadata.imports.push(ImportEntry {
                    request: RequestEntry::from_request(request),
                    phase: request.phase,
                    imported: ImportedName::Namespace,
                    local_slot: local_slot(bytecode, module, namespace)?,
                });
            }
            for item in named {
                metadata.imports.push(ImportEntry {
                    request: RequestEntry::from_request(request),
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
            let local_slot = local_slot(bytecode, module, &local)?;
            metadata.exports.push(ExportEntry::Local {
                exported: "default".into(),
                local_slot,
            });
            // An anonymous default class uses the synthetic `*default*`
            // binding, which is created during instantiation but remains in
            // the TDZ until ClassDeclaration evaluation. Anonymous default
            // functions are initialized earlier by declaration instantiation.
            if matches!(decl.as_ref(), Decl::Class(class) if class.name.is_none()) {
                metadata.uninitialized_slots.insert(local_slot);
            }
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
                    request: RequestEntry::from_request(request),
                    imported: item.local.value().to_string(),
                });
            }
        }
        ExportSpec::All { exported, request } => {
            push_request(metadata, request);
            if let Some(exported) = exported {
                metadata.exports.push(ExportEntry::Namespace {
                    exported: exported.value().to_string(),
                    request: RequestEntry::from_request(request),
                });
            } else {
                metadata.exports.push(ExportEntry::Star {
                    request: RequestEntry::from_request(request),
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
    let request = RequestEntry::from_request(request);
    if !metadata.requests.contains(&request) {
        metadata.requests.push(request);
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

pub(crate) fn fresh_module_environment(
    bytecode: &BytecodeModule,
    metadata: &ModuleMetadata,
) -> ModuleEnvironment {
    let bindings = (0..bytecode.main.locals.slot_count())
        .map(|slot| {
            if metadata.uninitialized_slots.contains(&usize::from(slot)) {
                Cell::uninitialized(!metadata.immutable_slots.contains(&usize::from(slot)))
            } else {
                Cell::mutable(Value::undefined())
            }
        })
        .collect();
    ModuleEnvironment { bindings }
}

fn normalize_key(key: &str) -> String {
    normalize_path(PathBuf::from(key))
}

/// Source text for a synthetic JSON Module Record.
///
/// Per `ParseJSONModule`, the default export is the value of parsing the source
/// with the engine's intrinsic JSON parser — *not* a realm-global `JSON.parse`
/// lookup that a dependency could have mutated before the module evaluates. The
/// pipeline therefore parses the source at load time
/// (`js_vm::builtins::parse_json_intrinsic`) and injects the value into the
/// `*default*` cell; the module's bytecode is never evaluated.
///
/// This skeleton only exists so the compiler allocates the `*default*` local and
/// a default-export entry (the AST analysis and linker need them). Its literal
/// value is irrelevant because evaluation is skipped.
pub(crate) const SYNTHETIC_JSON_MODULE_SOURCE: &str = "export default null;\n";

/// Build the source text of a synthetic text Module Record.
///
/// Per `CreateTextModule`: `CreateDefaultExportSyntheticModule(source)`. The
/// entire file contents become the single `default` export, never parsed as
/// JavaScript. Empty text yields `""`.
pub(crate) fn synthetic_text_module_source(original_source: &str) -> String {
    match serde_json::to_string(original_source) {
        Ok(quoted) => format!("export default {quoted};\n"),
        // serde_json only fails on non-UTF strings, which we already hold as a
        // Rust `str`; fall back to a hand encoding that escapes the characters
        // `to_string` would (defensive — not expected to trigger).
        Err(_) => format!(
            "export default {};\n",
            serde_json::to_string(&String::from_utf8_lossy(original_source.as_bytes()))
                .unwrap_or_else(|_| "\"\"".into())
        ),
    }
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
