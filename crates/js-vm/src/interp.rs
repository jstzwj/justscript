//! The dispatch loop.
//!
//! [`Interpreter::run_module`] pushes a frame for `<main>` and dispatches
//! instructions until a `Return` unwinds to the top. Function calls push new
//! frames; `Return` pops them and pushes the return value onto the caller's
//! operand stack — so the whole call tree runs inside one flat loop, no Rust
//! recursion.

use crate::frame::{new_cell, CallFrame};
use crate::{EngineFault, JsException, RuntimeError, RuntimeFrame};
use js_bytecode::{BytecodeFunction, BytecodeModule, Opcode};
use js_diagnostics::DiagResult;
use js_runtime::context::RealmContext;
use js_runtime::object::PropertyDescriptor;
use js_runtime::value::{
    GeneratorResumeKind, GeneratorState, GeneratorTryState, IteratorRecord, JsFunction, Value,
    ValueData,
};
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::rc::Rc;

pub(crate) type BytecodeGraph<'a> = [&'a BytecodeModule];

/// Native (Rust-implemented) function ids registered in the VM.
mod native_id {
    pub const GEN_NEXT: u16 = 0;
    pub const GEN_RETURN: u16 = 1;
    pub const GEN_THROW: u16 = 2;
}

/// Hidden property keys on array-iterator objects (source + cursor index).
const IT_SRC: &str = "__it_src";
const IT_IDX: &str = "__it_idx";

/// The result of a native call: either a plain value, or a request to resume a
/// paused generator with a normal, throw, or return completion.
pub enum NativeResult {
    Value(Value),
    ResumeGenerator(Rc<RefCell<GeneratorState>>, GeneratorResumeKind, Value),
    ResumeAsyncGenerator(
        Rc<RefCell<GeneratorState>>,
        GeneratorResumeKind,
        Value,
        js_runtime::object::JsObject,
    ),
}

/// Outcome of one [`Interpreter::step`]: keep going, or the running frame
/// returned (the top-level script in `dispatch`).
pub(crate) enum Step {
    More,
    Done(Value),
    Suspend(Value),
}

#[derive(Debug)]
pub enum ModuleExecution {
    Completed(Value),
    Suspended,
}

pub struct DynamicImportRequest {
    pub resolution: Result<usize, String>,
    pub promise: js_runtime::object::JsObject,
}

struct SuspendedModule {
    frames: Vec<CallFrame>,
    awaited: Value,
}

struct AsyncContinuation {
    frame: CallFrame,
}

pub(crate) enum PromiseJob {
    Reaction {
        reaction: js_runtime::object::PromiseReaction,
        argument: Value,
        rejected: bool,
    },
    ResolveThenable {
        promise: js_runtime::object::JsObject,
        thenable: Value,
        then: Value,
    },
    AsyncGeneratorRequest {
        generator: Rc<RefCell<GeneratorState>>,
        request: js_runtime::value::AsyncGeneratorRequest,
    },
}

struct DeferredModuleGraph {
    locals: Vec<Vec<js_runtime::value::Cell>>,
    dependencies: Vec<Vec<usize>>,
    evaluated: Vec<bool>,
}

/// A Rust-implemented builtin function. `this` is the call receiver (e.g. the
/// array for `arr.push`); `f` is the callee value (so generator methods can
/// read their bound generator).
pub trait NativeFn {
    fn call(
        &self,
        _interp: &mut Interpreter,
        modules: &BytecodeGraph<'_>,
        this: Value,
        f: &JsFunction,
        args: Vec<Value>,
        is_construct: bool,
    ) -> Result<NativeResult, InterpError>;
}

struct GenNext;
struct GenReturn;
struct GenThrow;

fn begin_async_generator_request(
    generator: &Rc<RefCell<GeneratorState>>,
    kind: GeneratorResumeKind,
    value: Value,
    promise: js_runtime::object::JsObject,
) -> bool {
    let mut state = generator.borrow_mut();
    if state.async_executing {
        state
            .async_queue
            .push_back(js_runtime::value::AsyncGeneratorRequest {
                kind,
                value,
                promise,
            });
        false
    } else {
        state.async_executing = true;
        true
    }
}

impl NativeFn for GenThrow {
    fn call(
        &self,
        _interp: &mut Interpreter,
        _modules: &BytecodeGraph<'_>,
        _this: Value,
        f: &JsFunction,
        args: Vec<Value>,
        _is_construct: bool,
    ) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let value = args.into_iter().next().unwrap_or_else(Value::undefined);
        let (is_async, done, started) = {
            let state = gen.borrow();
            (state.is_async, state.done, state.started)
        };
        if done || !started {
            gen.borrow_mut().done = true;
            if is_async {
                return Ok(NativeResult::Value(crate::builtins::promise_rejected(
                    value,
                )));
            }
            return Err(InterpError::Throw(value));
        }
        if is_async {
            let promise = crate::builtins::promise_pending();
            if !begin_async_generator_request(
                &gen,
                GeneratorResumeKind::Throw,
                value.clone(),
                promise.clone(),
            ) {
                return Ok(NativeResult::Value(Value::object(promise)));
            }
            Ok(NativeResult::ResumeAsyncGenerator(
                gen,
                GeneratorResumeKind::Throw,
                value,
                promise,
            ))
        } else {
            Ok(NativeResult::ResumeGenerator(
                gen,
                GeneratorResumeKind::Throw,
                value,
            ))
        }
    }
}

impl NativeFn for GenNext {
    fn call(
        &self,
        interp: &mut Interpreter,
        modules: &BytecodeGraph<'_>,
        _this: Value,
        f: &JsFunction,
        args: Vec<Value>,
        _is_construct: bool,
    ) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let arg = args.into_iter().next().unwrap_or_else(Value::undefined);
        if gen.borrow().is_async {
            if gen.borrow().done {
                let promise = crate::builtins::promise_resolved(
                    interp,
                    modules,
                    iter_result(Value::undefined(), true),
                )?;
                return Ok(NativeResult::Value(promise));
            }
            let promise = crate::builtins::promise_pending();
            if !begin_async_generator_request(
                &gen,
                GeneratorResumeKind::Next,
                arg.clone(),
                promise.clone(),
            ) {
                return Ok(NativeResult::Value(Value::object(promise)));
            }
            Ok(NativeResult::ResumeAsyncGenerator(
                gen,
                GeneratorResumeKind::Next,
                arg,
                promise,
            ))
        } else {
            Ok(NativeResult::ResumeGenerator(
                gen,
                GeneratorResumeKind::Next,
                arg,
            ))
        }
    }
}

impl NativeFn for GenReturn {
    fn call(
        &self,
        interp: &mut Interpreter,
        modules: &BytecodeGraph<'_>,
        _this: Value,
        f: &JsFunction,
        args: Vec<Value>,
        _is_construct: bool,
    ) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let value = args.into_iter().next().unwrap_or_else(Value::undefined);
        let (is_async, done, started) = {
            let state = gen.borrow();
            (state.is_async, state.done, state.started)
        };
        if done || !started {
            gen.borrow_mut().done = true;
            let result = iter_result(value, true);
            if is_async {
                return Ok(NativeResult::Value(crate::builtins::promise_resolved(
                    interp, modules, result,
                )?));
            }
            return Ok(NativeResult::Value(result));
        }
        if is_async {
            let promise = crate::builtins::promise_pending();
            if !begin_async_generator_request(
                &gen,
                GeneratorResumeKind::Return,
                value.clone(),
                promise.clone(),
            ) {
                return Ok(NativeResult::Value(Value::object(promise)));
            }
            Ok(NativeResult::ResumeAsyncGenerator(
                gen,
                GeneratorResumeKind::Return,
                value,
                promise,
            ))
        } else {
            Ok(NativeResult::ResumeGenerator(
                gen,
                GeneratorResumeKind::Return,
                value,
            ))
        }
    }
}

/// A runtime error surfaced from the interpreter.
#[derive(Debug)]
pub enum InterpError {
    /// A user-visible JavaScript throw.
    Throw(Value),
    /// A VM bug / unimplemented opcode.
    Internal(String),
}

impl fmt::Display for InterpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterpError::Throw(v) => write!(f, "Uncaught {}", to_string(v)),
            InterpError::Internal(msg) => write!(f, "internal interpreter error: {msg}"),
        }
    }
}

impl std::error::Error for InterpError {}

/// The bytecode interpreter.
pub struct Interpreter {
    ctx: RealmContext,
    frames: Vec<CallFrame>,
    /// Registered native (Rust) builtins, indexed by `JsFunction.native`.
    natives: Vec<Box<dyn NativeFn>>,
    /// A native-call error surfaced to the dispatch loop (which checks it and
    /// unwinds). `None` while no error is pending.
    pending_err: Option<InterpError>,
    /// Instructions executed so far. Bounded by [`MAX_STEPS`] so a runaway loop
    /// (a VM bug or an infinite-loop test) terminates as `Internal` instead of
    /// hanging the conformance runner.
    steps: u64,
    /// Frames already crossed by the currently propagating error. This is
    /// needed for native callbacks, whose bytecode frames unwind before the
    /// error returns to the outer dispatch loop.
    error_trace: Vec<RuntimeFrame>,
    /// PromiseJobs run FIFO at explicit microtask checkpoints.
    jobs: VecDeque<PromiseJob>,
    suspended_modules: HashMap<usize, SuspendedModule>,
    dynamic_imports: Option<Vec<Vec<Result<usize, String>>>>,
    dynamic_import_requests: VecDeque<DynamicImportRequest>,
    deferred_modules: Option<DeferredModuleGraph>,
    /// Runtime-compiled eval modules. Box allocation keeps bytecode addresses
    /// stable while the table grows and closures outlive the eval call.
    eval_modules: Vec<(usize, Box<BytecodeModule>)>,
    next_private_brand: u64,
    suspended_async: HashMap<u64, AsyncContinuation>,
    next_async_continuation: u64,
    template_objects: HashMap<(usize, usize, u16), Value>,
    import_meta_objects: HashMap<usize, Value>,
}

/// Hard cap on instructions per program. Generous enough for legitimate
/// programs, small enough to bound a stuck run (each exhausted budget costs a
/// fraction of a second, which matters when many tests hit it).
const MAX_STEPS: u64 = 5_000_000;
/// Maximum nested call depth. JS recursion drives Rust recursion through
/// `step → invoke → call_value → step`, so this bounds the native stack and
/// prevents a stack-overflow abort on deep (or runaway) recursion.
const MAX_FRAMES: usize = 2000;

impl Interpreter {
    pub fn new(ctx: RealmContext) -> Interpreter {
        let interp = Interpreter {
            ctx,
            frames: Vec::new(),
            natives: default_natives(),
            pending_err: None,
            steps: 0,
            error_trace: Vec::new(),
            jobs: VecDeque::new(),
            suspended_modules: HashMap::new(),
            dynamic_imports: None,
            dynamic_import_requests: VecDeque::new(),
            deferred_modules: None,
            eval_modules: Vec::new(),
            next_private_brand: 1,
            suspended_async: HashMap::new(),
            next_async_continuation: 1,
            template_objects: HashMap::new(),
            import_meta_objects: HashMap::new(),
        };
        // Install the VM intrinsics (global builtins, `globalThis`, the
        // per-realm intrinsic prototypes, and the `Array.prototype` wiring)
        // ONCE per realm. A realm is long-lived and is shared across every
        // interpreter this engine creates (`Engine::run` / `run_module` make a
        // fresh interpreter per call), so re-installing per interpreter would
        // (a) mint a fresh `%ObjectPrototype%` / `%ArrayPrototype%` on every
        // execute — breaking `getPrototypeOf(objFromExecute1) ===
        // Array.prototype` in execute 2 — and (b) overwrite user modifications
        // to built-ins between executes.
        {
            let mut realm = interp.ctx.realm.borrow_mut();
            if !realm.intrinsics_initialized {
                crate::builtins::install_globals(&mut realm.globals);
                let global_this = Value::object(realm.global_object.clone());
                realm.globals.insert("globalThis".into(), global_this);
                // `%ObjectPrototype%` is the property the installer left on the
                // Object constructor namespace.
                let object_proto_value = realm
                    .globals
                    .get("Object")
                    .map(|ctor| get_property(ctor, &Value::string("prototype")));
                realm.object_proto = object_proto_value.as_ref().and_then(obj_as_object).cloned();
                // `%ArrayPrototype%` is a fresh ordinary object whose
                // `[[Prototype]]` is `%ObjectPrototype%`
                // (sec-properties-of-the-array-prototype-object), so
                // `Object.getPrototypeOf(Array.prototype) === Object.prototype`.
                let object_proto = realm.object_proto.clone();
                let array_proto = js_runtime::object::ObjectData::new_handle();
                array_proto.borrow_mut().proto = object_proto.map(Value::object);
                realm.array_proto = Some(array_proto.clone());
                // Wire `Array.prototype` to this realm's `%ArrayPrototype%` as a
                // real property on the Array constructor (mirroring
                // `Object.prototype`), so it resolves through the ordinary
                // property walk — no thread-local, so two interpreters on one
                // thread never cross-contaminate prototypes.
                if let Some(array_ctor) = realm.globals.get("Array").and_then(obj_as_object) {
                    array_ctor.borrow_mut().properties.insert(
                        "prototype".into(),
                        js_runtime::object::PropertyDescriptor::Data {
                            value: Value::object(array_proto),
                            attr: js_runtime::object::Attribute {
                                writable: true,
                                enumerable: false,
                                configurable: false,
                            },
                        },
                    );
                }
                realm.intrinsics_initialized = true;
            }
        }
        interp
    }

    /// Construct an interpreter with a fresh realm.
    pub fn fresh() -> Interpreter {
        Interpreter::new(RealmContext::fresh())
    }

    fn module_ptr(
        &self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Option<*const BytecodeModule> {
        modules
            .get(module_index)
            .map(|module| *module as *const BytecodeModule)
            .or_else(|| {
                self.eval_modules
                    .iter()
                    .find(|(index, _)| *index == module_index)
                    .map(|(_, module)| module.as_ref() as *const BytecodeModule)
            })
    }

    fn module_ref<'a>(
        &'a self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Option<&'a BytecodeModule> {
        let pointer = self.module_ptr(modules, module_index)?;
        // Bytecode modules in the external graph live for the dispatch call;
        // eval modules are boxed and never removed from this interpreter.
        Some(unsafe { &*pointer })
    }

    /// Register the instantiated module graph used by deferred namespace
    /// objects. Ordinary dependencies are evaluated recursively when a
    /// deferred namespace first becomes observable.
    pub fn configure_module_graph(
        &mut self,
        locals: Vec<Vec<js_runtime::value::Cell>>,
        dependencies: Vec<Vec<usize>>,
    ) {
        let evaluated = vec![false; locals.len()];
        self.deferred_modules = Some(DeferredModuleGraph {
            locals,
            dependencies,
            evaluated,
        });
    }

    pub fn configure_dynamic_imports(&mut self, resolutions: Vec<Vec<Result<usize, String>>>) {
        self.dynamic_imports = Some(resolutions);
    }

    pub fn take_dynamic_import_requests(&mut self) -> Vec<DynamicImportRequest> {
        self.dynamic_import_requests.drain(..).collect()
    }

    pub fn has_dynamic_import_requests(&self) -> bool {
        !self.dynamic_import_requests.is_empty()
    }

    pub fn has_promise_jobs(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// The current realm's `%ArrayPrototype%`, for ordinary array construction.
    /// Read straight from the realm so each interpreter's arrays link to its own
    /// prototype — no thread-local, so two interpreters on one thread stay
    /// isolated. Returns `None` only before realm bootstrapping sets the field.
    pub fn array_prototype(&self) -> Option<Value> {
        self.ctx
            .realm
            .borrow()
            .array_proto
            .as_ref()
            .map(|handle| Value::object(handle.clone()))
    }

    /// The current realm's `%ObjectPrototype%`, for ordinary object allocation.
    pub fn object_prototype(&self) -> Option<Value> {
        self.ctx
            .realm
            .borrow()
            .object_proto
            .as_ref()
            .map(|handle| Value::object(handle.clone()))
    }

    pub fn resolve_host_promise(
        &mut self,
        modules: &BytecodeGraph<'_>,
        promise: js_runtime::object::JsObject,
        value: Value,
    ) -> Result<(), InterpError> {
        crate::builtins::resolve_promise(self, modules, promise, value)
    }

    pub fn resolve_host_promise_report(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
        promise: js_runtime::object::JsObject,
        value: Value,
    ) -> Result<(), RuntimeError> {
        self.resolve_host_promise(modules, promise, value)
            .map_err(|error| self.runtime_error(modules, module_index, error))
    }

    pub fn reject_host_promise(&mut self, promise: js_runtime::object::JsObject, reason: Value) {
        crate::builtins::reject_promise(self, promise, reason);
    }

    pub fn mark_module_evaluated(&mut self, module_index: usize) {
        if let Some(graph) = &mut self.deferred_modules {
            if let Some(evaluated) = graph.evaluated.get_mut(module_index) {
                *evaluated = true;
            }
        }
    }

    /// Instantiate a top-level function declaration against an already-created
    /// module environment. This is used during module linking, before any body
    /// evaluation, as required by ModuleDeclarationInstantiation.
    pub fn instantiate_module_function(
        module: &BytecodeModule,
        module_index: usize,
        function_id: u32,
        locals: &[js_runtime::value::Cell],
    ) -> Result<Value, String> {
        let function = func_ref(module, function_id as usize);
        let mut upvalues = Vec::with_capacity(function.upvalues.len());
        for spec in &function.upvalues {
            if !spec.is_local {
                return Err(format!(
                    "top-level function {} has a non-local upvalue",
                    function.name
                ));
            }
            let cell = locals.get(spec.index as usize).cloned().ok_or_else(|| {
                format!(
                    "top-level function {} captures missing slot {}",
                    function.name, spec.index
                )
            })?;
            upvalues.push(cell);
        }
        let mut value = JsFunction::new(function.name.clone(), function_id, function.param_count);
        value.module_index = module_index as u32;
        value.upvalues = upvalues;
        value.is_generator = function.is_generator;
        Ok(Value::function(value))
    }

    /// Record Test262's `$DONE` signal. When `error` is `Some`, `$DONE` was
    /// invoked with a failure value; that value is retained so the host runner
    /// can classify the test as failed even though the throw is swallowed by
    /// the surrounding Promise reaction. First error wins.
    pub(crate) fn mark_test262_done(&mut self, error: Option<Value>) {
        let mut realm = self.ctx.realm.borrow_mut();
        realm.test262_done_called = true;
        if realm.test262_done_error.is_none() {
            realm.test262_done_error = error;
        }
    }

    /// Execute a compiled module's top-level function.
    pub fn run_module(&mut self, module: &BytecodeModule) -> Result<Value, InterpError> {
        let modules = [module];
        let locals = (0..module.main.locals.slot_count())
            .map(|_| new_cell(Value::undefined()))
            .collect();
        self.run_module_in_graph(&modules, 0, locals)
    }

    /// Execute one top-level module using pre-instantiated environment cells.
    /// Functions created during execution retain `module_index`, so imports can
    /// call bytecode defined in another module from the same graph.
    pub fn run_module_in_graph(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
        locals: Vec<js_runtime::value::Cell>,
    ) -> Result<Value, InterpError> {
        let Some(module) = modules.get(module_index).copied() else {
            return Err(InterpError::Internal(format!(
                "module index {module_index} is outside the bytecode graph"
            )));
        };
        if locals.len() != module.main.locals.slot_count() as usize {
            return Err(InterpError::Internal(format!(
                "module environment has {} cells, expected {}",
                locals.len(),
                module.main.locals.slot_count()
            )));
        }
        if let Err(errors) = js_bytecode::verify_module(module) {
            let detail = errors
                .iter()
                .take(3)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(InterpError::Internal(format!("invalid bytecode: {detail}")));
        }
        let mut outcome = self.start_module_in_graph(modules, module_index, locals)?;
        loop {
            match outcome {
                ModuleExecution::Completed(value) => return Ok(value),
                ModuleExecution::Suspended => {
                    self.drain_jobs(modules)?;
                    outcome = self.resume_module_in_graph(modules, module_index)?;
                }
            }
        }
    }

    pub fn start_module_in_graph(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
        locals: Vec<js_runtime::value::Cell>,
    ) -> Result<ModuleExecution, InterpError> {
        let module = modules.get(module_index).copied().ok_or_else(|| {
            InterpError::Internal(format!(
                "module index {module_index} is outside the bytecode graph"
            ))
        })?;
        if locals.len() != module.main.locals.slot_count() as usize {
            return Err(InterpError::Internal(format!(
                "module environment has {} cells, expected {}",
                locals.len(),
                module.main.locals.slot_count()
            )));
        }
        if self.suspended_modules.contains_key(&module_index) {
            return Err(InterpError::Internal(format!(
                "module {module_index} is already suspended"
            )));
        }
        self.error_trace.clear();
        self.steps = 0;
        self.frames.clear();
        let mut frame = CallFrame::with_locals(module_index, 0, locals, module.main.span);
        if !module.is_module {
            let global_this = Value::object(self.ctx.realm.borrow().global_object.clone());
            frame.this = global_this.clone();
            frame.this_binding = js_runtime::value::Cell::mutable(global_this);
        }
        self.frames.push(frame);
        self.dispatch_module(modules, module_index)
    }

    pub fn start_module_in_graph_report(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
        locals: Vec<js_runtime::value::Cell>,
    ) -> Result<ModuleExecution, RuntimeError> {
        self.start_module_in_graph(modules, module_index, locals)
            .map_err(|error| self.runtime_error(modules, module_index, error))
    }

    pub fn resume_module_in_graph(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Result<ModuleExecution, InterpError> {
        let suspended = self
            .suspended_modules
            .remove(&module_index)
            .ok_or_else(|| {
                InterpError::Internal(format!("module {module_index} is not suspended"))
            })?;
        match crate::builtins::promise_result(&suspended.awaited) {
            Some(crate::builtins::AwaitedPromise::Pending) => {
                self.suspended_modules.insert(module_index, suspended);
                return Ok(ModuleExecution::Suspended);
            }
            Some(crate::builtins::AwaitedPromise::Fulfilled(value)) => {
                self.frames = suspended.frames;
                self.top().stack.push(value);
            }
            Some(crate::builtins::AwaitedPromise::Rejected(value)) => {
                self.frames = suspended.frames;
                self.handle_exception(modules, InterpError::Throw(value), 0)?;
            }
            None => unreachable!("await continuation must hold a Promise"),
        }
        self.dispatch_module(modules, module_index)
    }

    pub fn resume_module_in_graph_report(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Result<ModuleExecution, RuntimeError> {
        self.resume_module_in_graph(modules, module_index)
            .map_err(|error| self.runtime_error(modules, module_index, error))
    }

    pub fn module_is_ready(&self, module_index: usize) -> bool {
        self.suspended_modules
            .get(&module_index)
            .is_some_and(|suspended| {
                !matches!(
                    crate::builtins::promise_result(&suspended.awaited),
                    Some(crate::builtins::AwaitedPromise::Pending)
                )
            })
    }

    pub fn run_promise_jobs(&mut self, modules: &BytecodeGraph<'_>) -> Result<(), InterpError> {
        self.drain_jobs(modules)
    }

    pub fn run_promise_jobs_report(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Result<(), RuntimeError> {
        self.run_promise_jobs(modules)
            .map_err(|error| self.runtime_error(modules, module_index, error))
    }

    fn runtime_error(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
        error: InterpError,
    ) -> RuntimeError {
        let fallback = modules
            .get(module_index)
            .and_then(|module| module.source.clone());
        let stack = std::mem::take(&mut self.error_trace);
        let source = stack
            .first()
            .and_then(|frame| frame.source.clone())
            .or(fallback);
        match error {
            InterpError::Throw(value) => RuntimeError::Exception(JsException {
                value,
                source,
                stack,
            }),
            InterpError::Internal(message) => {
                RuntimeError::Fault(EngineFault::new(message, source, stack))
            }
        }
    }

    fn dispatch_module(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Result<ModuleExecution, InterpError> {
        match self.dispatch(modules)? {
            Step::Done(value) => Ok(ModuleExecution::Completed(value)),
            Step::Suspend(awaited) => {
                let frames = std::mem::take(&mut self.frames);
                self.suspended_modules
                    .insert(module_index, SuspendedModule { frames, awaited });
                Ok(ModuleExecution::Suspended)
            }
            Step::More => unreachable!(),
        }
    }

    /// Execute a module and retain source identity, throw location and the
    /// JavaScript call stack when execution does not complete normally.
    pub fn run_module_report(&mut self, module: &BytecodeModule) -> Result<Value, RuntimeError> {
        let modules = [module];
        let locals = (0..module.main.locals.slot_count())
            .map(|_| new_cell(Value::undefined()))
            .collect();
        self.run_module_in_graph_report(&modules, 0, locals)
    }

    pub fn run_module_in_graph_report(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
        locals: Vec<js_runtime::value::Cell>,
    ) -> Result<Value, RuntimeError> {
        let source = modules
            .get(module_index)
            .and_then(|module| module.source.clone());
        match self.run_module_in_graph(modules, module_index, locals) {
            Ok(value) => Ok(value),
            Err(InterpError::Throw(value)) => {
                let stack = std::mem::take(&mut self.error_trace);
                let source = stack
                    .first()
                    .and_then(|frame| frame.source.clone())
                    .or(source);
                Err(RuntimeError::Exception(JsException {
                    value,
                    source,
                    stack,
                }))
            }
            Err(InterpError::Internal(message)) => {
                let stack = std::mem::take(&mut self.error_trace);
                let source = stack
                    .first()
                    .and_then(|frame| frame.source.clone())
                    .or(source);
                Err(RuntimeError::Fault(EngineFault::new(
                    message, source, stack,
                )))
            }
        }
    }

    /// Main dispatch loop: runs instructions until the top-level frame returns.
    /// Exceptions surface as `Err` from [`Self::step`]; [`Self::handle_exception`]
    /// either installs a catch/finally (continue) or propagates (pop frame).
    fn dispatch(&mut self, modules: &BytecodeGraph<'_>) -> Result<Step, InterpError> {
        loop {
            match self.step(modules) {
                Ok(Step::More) => {}
                Ok(done @ (Step::Done(_) | Step::Suspend(_))) => return Ok(done),
                Err(e) => match self.handle_exception(modules, e, 0) {
                    Ok(()) => {}
                    Err(e) => return Err(e),
                },
            }
        }
    }

    /// Try to handle an exception: pop frames (down to `stop_depth`) until one
    /// has an active catch/finally to install; if none, propagate the exception
    /// (return `Err`). `dispatch` uses stop_depth 0; `call_value` uses its
    /// caller-depth target so it never touches the suspended caller frame.
    fn handle_exception(
        &mut self,
        modules: &BytecodeGraph<'_>,
        e: InterpError,
        stop_depth: usize,
    ) -> Result<(), InterpError> {
        let thrown = match e {
            InterpError::Throw(v) => v,
            other => {
                // Internal errors are never catchable. Capture the intact stack
                // once before returning it to the host.
                if self.error_trace.is_empty() {
                    self.error_trace = self
                        .frames
                        .iter()
                        .rev()
                        .filter_map(|frame| {
                            self.module_ref(modules, frame.module_index)
                                .map(|module| runtime_frame(module, frame))
                        })
                        .collect();
                }
                return Err(other);
            }
        };
        while self.frames.len() > stop_depth {
            let handler = self.frames.last_mut().unwrap().try_stack.pop();
            match handler {
                Some(h) => {
                    let frame = self.frames.last_mut().unwrap();
                    if let Some(catch_pc) = h.catch_pc {
                        // A catch handles this propagation. Any frames
                        // accumulated while crossing a native callback no
                        // longer describe an escaping exception.
                        self.error_trace.clear();
                        frame.pc = catch_pc as usize;
                        frame.stack.push(thrown);
                        frame.pending_throw = None; // caught
                    } else if let Some(finally_pc) = h.finally_pc {
                        frame.pc = finally_pc as usize;
                        frame.pending_throw = Some(thrown); // re-raise after finally
                    }
                    return Ok(());
                }
                None => {
                    let frame = self.frames.last().unwrap();
                    let module = self
                        .module_ref(modules, frame.module_index)
                        .ok_or_else(|| {
                            InterpError::Internal(format!(
                                "frame refers to missing bytecode module {}",
                                frame.module_index
                            ))
                        })?;
                    if let Some(generator) = frame.generator.clone() {
                        {
                            let mut state = generator.borrow_mut();
                            state.done = true;
                            state.delegate = None;
                        }
                        let active_promise = frame.async_generator_promise.clone();
                        if let Some(promise) = active_promise {
                            crate::builtins::reject_promise(self, promise.clone(), thrown);
                            self.frames.pop();
                            self.finish_async_generator_request(generator);
                            if let Some(caller) = self.frames.last_mut() {
                                caller.stack.push(Value::object(promise));
                                return Ok(());
                            }
                            return Ok(());
                        }
                    } else if let Some(promise) = frame.async_promise.clone() {
                        crate::builtins::reject_promise(self, promise.clone(), thrown);
                        self.frames.pop();
                        if let Some(caller) = self.frames.last_mut() {
                            caller.stack.push(Value::object(promise));
                            return Ok(());
                        }
                        return Ok(());
                    }
                    self.error_trace.push(runtime_frame(module, frame));
                    self.frames.pop(); // no handler here → unwind to caller
                }
            }
        }
        Err(InterpError::Throw(thrown))
    }

    /// One-instruction outcome of [`Self::step`]: keep going, or the running
    /// frame returned `Value` (the top-level script, in [`Self::dispatch`]).
    fn step(&mut self, modules: &BytecodeGraph<'_>) -> Result<Step, InterpError> {
        // Step budget: bound runaway loops so a stuck program surfaces as an
        // `Internal` error instead of hanging the runner.
        self.steps = self.steps.saturating_add(1);
        if self.steps > MAX_STEPS {
            return Err(InterpError::Internal(
                "step budget exceeded (possible infinite loop)".into(),
            ));
        }
        // A native call may have surfaced an error to unwind the loop.
        if let Some(err) = self.pending_err.take() {
            return Err(err);
        }
        // A plain `yield` resumes at the following instruction. Apply its
        // completion before fetching that instruction; delegated yields replay
        // `YieldStar`, which consumes the completion itself.
        let plain_resume = self.frames.last_mut().and_then(|frame| {
            let delegated = frame
                .generator
                .as_ref()
                .is_some_and(|generator| generator.borrow().delegate.is_some());
            (!delegated)
                .then(|| frame.generator_resume.take())
                .flatten()
        });
        if let Some((kind, value)) = plain_resume {
            match kind {
                GeneratorResumeKind::Next => self.top().stack.push(value),
                GeneratorResumeKind::Throw => return Err(InterpError::Throw(value)),
                GeneratorResumeKind::Return => return self.complete_generator_request(value),
            }
        }
        let module_index = self.frames.last().unwrap().module_index;
        let module_ptr = self.module_ptr(modules, module_index).ok_or_else(|| {
            InterpError::Internal(format!(
                "frame refers to missing bytecode module {module_index}"
            ))
        })?;
        let module = unsafe { &*module_ptr };
        // Fetch + advance the PC without holding a long-lived borrow.
        let ins = {
            let frame = self.frames.last_mut().unwrap();
            let func = func_ref(module, frame.func_index);
            let pc = frame.pc;
            match func.code.get(pc) {
                Some(ins) => {
                    frame.pc += 1;
                    frame.span = func.source_span(pc);
                    *ins
                }
                None => {
                    // Fell off the end of a function without an explicit
                    // Return — treat as `return undefined`.
                    return Ok(Step::Done(Value::undefined()));
                }
            }
        };

        match ins.op {
            Opcode::Nop => {}
            Opcode::ArrayPush => {
                let v = self.top().stack.pop();
                let arr = self.top().stack.peek().clone();
                array_append(&arr, v);
            }
            Opcode::ArrayExtend => {
                let src = self.top().stack.pop();
                let arr = self.top().stack.peek().clone();
                let values: Vec<Value> = match src.data() {
                    ValueData::Object(o) => {
                        let b = o.borrow();
                        let len = b
                            .properties
                            .get("length")
                            .and_then(|d| match d {
                                js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                                    Some(value.clone())
                                }
                                _ => None,
                            })
                            .and_then(|v| match v.data() {
                                ValueData::Integer(i) => Some(*i as usize),
                                ValueData::Number(n) => Some(*n as usize),
                                _ => None,
                            })
                            .unwrap_or(0);
                        (0..len)
                            .filter_map(|i| {
                                b.properties.get(&i.to_string()).and_then(|d| match d {
                                    js_runtime::object::PropertyDescriptor::Data {
                                        value, ..
                                    } => Some(value.clone()),
                                    _ => None,
                                })
                            })
                            .collect()
                    }
                    ValueData::String(s) => s
                        .as_str()
                        .chars()
                        .map(|c| Value::string(c.to_string()))
                        .collect(),
                    _ => Vec::new(),
                };
                for v in values {
                    array_append(&arr, v);
                }
            }
            Opcode::GetIterator => {
                let v = self.top().stack.pop();
                let record = self.get_iterator_record(modules, v, false)?;
                self.top().stack.push(iterator_record_value(record));
            }
            Opcode::IterNext => {
                let record_value = self.top().stack.pop();
                let record_object = obj_as_object(&record_value).cloned().ok_or_else(|| {
                    InterpError::Internal("iterator local is not an object".into())
                })?;
                let record = record_object
                    .borrow()
                    .iterator_record
                    .clone()
                    .ok_or_else(|| {
                        InterpError::Internal("iterator local has no Iterator Record".into())
                    })?;
                let result = if record.done {
                    iter_result(Value::undefined(), true)
                } else if record.intrinsic_next {
                    step_array_iterator(&record.iterator)
                } else {
                    self.call_value(
                        modules,
                        record.next_method.clone(),
                        Vec::new(),
                        record.iterator.clone(),
                    )?
                };
                if !result.is_object() {
                    record_object
                        .borrow_mut()
                        .iterator_record
                        .as_mut()
                        .unwrap()
                        .done = true;
                    return Err(InterpError::Throw(type_error(
                        "iterator result is not an object",
                    )));
                }
                self.top().stack.push(result);
            }
            Opcode::ObjectKeys => {
                let v = self.top().stack.pop();
                self.ensure_deferred_namespace(modules, &v)?;
                let keys: Vec<String> = match v.data() {
                    ValueData::Object(o) => {
                        let b = o.borrow();
                        // For arrays, expose numeric indices (skip "length").
                        if let Some(namespace) = &b.module_namespace {
                            if namespace.values().any(|binding| !binding.is_initialized()) {
                                return Err(InterpError::Throw(type_error_named(
                                    "ReferenceError",
                                    "cannot access binding before initialization",
                                )));
                            }
                            namespace.keys().cloned().collect()
                        } else if b.is_exotic_array {
                            (0..b.properties.len().saturating_sub(1))
                                .map(|i| i.to_string())
                                .collect()
                        } else {
                            b.properties.keys().cloned().collect()
                        }
                    }
                    ValueData::String(s) => (0..s.chars().count()).map(|i| i.to_string()).collect(),
                    _ => Vec::new(),
                };
                let o = js_runtime::object::ObjectData::new_handle();
                {
                    let mut obj = o.borrow_mut();
                    obj.class = "Array";
                    obj.is_exotic_array = true;
                    for (i, k) in keys.iter().enumerate() {
                        obj.properties.insert(
                            i.to_string(),
                            js_runtime::object::PropertyDescriptor::data(Value::string(k.clone())),
                        );
                    }
                    obj.properties.insert(
                        "length".to_string(),
                        js_runtime::object::PropertyDescriptor::data(Value::integer(
                            keys.len() as i32
                        )),
                    );
                }
                self.top().stack.push(Value::object(o));
            }

            // ---- stack / constants ----
            Opcode::LdaUndefined => self.top().stack.push(Value::undefined()),
            Opcode::LdaNull => self.top().stack.push(Value::null()),
            Opcode::LdaTrue => self.top().stack.push(Value::boolean(true)),
            Opcode::LdaFalse => self.top().stack.push(Value::boolean(false)),
            Opcode::LdaConst => {
                let v = module.constants.get(ins.operand).clone();
                self.top().stack.push(v);
            }
            Opcode::LdaFunction => {
                let f = self.function_value(module, module_index, ins.operand as u32);
                self.top().stack.push(Value::function(f));
            }
            Opcode::SetFunctionName => {
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(name) => name.as_str().to_string(),
                    _ => {
                        return Err(InterpError::Internal(
                            "SetFunctionName operand is not a string".into(),
                        ))
                    }
                };
                let mut value = self.top().stack.pop();
                if let Some(function) = value.as_function_mut() {
                    function.name = name.clone();
                    function.object.borrow_mut().properties.insert(
                        "name".into(),
                        PropertyDescriptor::Data {
                            value: Value::string(name),
                            attr: js_runtime::object::Attribute {
                                writable: false,
                                enumerable: false,
                                configurable: true,
                            },
                        },
                    );
                }
                self.top().stack.push(value);
            }
            Opcode::LdaThis => {
                let v = self.current_this()?;
                self.top().stack.push(v);
            }
            Opcode::LdaUpvalue => {
                let v = self.top().upvalues[ins.operand as usize]
                    .get()
                    .map_err(binding_error_value)?;
                self.top().stack.push(v);
            }
            Opcode::StaUpvalue => {
                let v = self.top().stack.pop();
                self.top().upvalues[ins.operand as usize]
                    .set(v)
                    .map_err(binding_error_value)?;
            }
            Opcode::LdaLocal => {
                let v = self.top().locals[ins.operand as usize]
                    .get()
                    .map_err(binding_error_value)?;
                self.top().stack.push(v);
            }
            Opcode::StaLocal => {
                let v = self.top().stack.pop();
                self.top().locals[ins.operand as usize]
                    .set(v)
                    .map_err(binding_error_value)?;
            }
            Opcode::Pop => {
                self.top().stack.pop();
            }
            Opcode::Dup => {
                let v = self.top().stack.peek().clone();
                self.top().stack.push(v);
            }
            Opcode::Swap => {
                let b = self.top().stack.pop();
                let a = self.top().stack.pop();
                self.top().stack.push(b);
                self.top().stack.push(a);
            }
            Opcode::SetClassHeritage => {
                let mut class = self.top().stack.pop();
                let superclass = self.top().stack.pop();
                if !superclass.is_null() && !superclass.is_function() {
                    return Err(InterpError::Throw(type_error(
                        "class extends value is not a constructor or null",
                    )));
                }
                if let (Some(class_object), Some(super_object)) =
                    (obj_as_object(&class), obj_as_object(&superclass))
                {
                    class_object.borrow_mut().proto = Some(superclass.clone());
                    let class_prototype = class_object
                        .borrow()
                        .properties
                        .get("prototype")
                        .and_then(|descriptor| match descriptor {
                            PropertyDescriptor::Data { value, .. } => Some(value),
                            PropertyDescriptor::Accessor { .. } => None,
                        })
                        .cloned();
                    let super_prototype = super_object
                        .borrow()
                        .properties
                        .get("prototype")
                        .and_then(|descriptor| match descriptor {
                            PropertyDescriptor::Data { value, .. } => Some(value),
                            PropertyDescriptor::Accessor { .. } => None,
                        })
                        .cloned();
                    if let (Some(class_prototype), Some(super_prototype)) =
                        (class_prototype, super_prototype)
                    {
                        if let Some(class_prototype) = obj_as_object(&class_prototype) {
                            class_prototype.borrow_mut().proto = Some(super_prototype);
                        }
                    }
                }
                let function = class.as_function_mut().unwrap();
                function.superclass = Some(Box::new(superclass.clone()));
                if let Some(initializer) = function.instance_initializer.as_deref_mut() {
                    if let Some(initializer) = initializer.as_function_mut() {
                        initializer.superclass = Some(Box::new(superclass));
                    }
                }
                self.top().stack.push(class);
            }
            Opcode::SetClassInstanceInitializer => {
                let mut initializer = self.top().stack.pop();
                let mut class = self.top().stack.pop();
                let class_brands = class
                    .as_function()
                    .map(|function| function.private_brands.clone())
                    .ok_or_else(|| {
                        InterpError::Internal(
                            "class instance initializer target is not a function".into(),
                        )
                    })?;
                let initializer_function = initializer.as_function_mut().ok_or_else(|| {
                    InterpError::Internal("class instance initializer is not a function".into())
                })?;
                initializer_function.private_brands.extend(class_brands);
                initializer_function.class_field_keys =
                    class.as_function().unwrap().class_field_keys.clone();
                initializer_function.home_object =
                    Some(Box::new(get_property(&class, &Value::string("prototype"))));
                class.as_function_mut().unwrap().instance_initializer = Some(Box::new(initializer));
                self.top().stack.push(class);
            }
            Opcode::DefineClassFieldKey => {
                let key = self.top().stack.pop();
                let class = self.top().stack.pop();
                let key = self.to_property_key_value(modules, key)?;
                let function = class.as_function().ok_or_else(|| {
                    InterpError::Internal("computed class key target is not a function".into())
                })?;
                function.class_field_keys.borrow_mut().push(key);
                self.top().stack.push(class);
            }
            Opcode::LoadClassFieldKey => {
                let key = self
                    .frames
                    .last()
                    .unwrap()
                    .class_field_keys
                    .borrow()
                    .get(ins.operand as usize)
                    .cloned()
                    .ok_or_else(|| {
                        InterpError::Internal(format!(
                            "computed class key {} was not initialized",
                            ins.operand
                        ))
                    })?;
                self.top().stack.push(key);
            }
            Opcode::ActivateClassPrivateEnvironment => {
                let brands = self
                    .top()
                    .stack
                    .peek()
                    .as_function()
                    .ok_or_else(|| {
                        InterpError::Internal(
                            "class private environment target is not a function".into(),
                        )
                    })?
                    .private_brands
                    .clone();
                let frame = self.top();
                frame
                    .private_environment_stack
                    .push(frame.private_brands.clone());
                frame.private_brands.extend(brands);
            }
            Opcode::DeactivateClassPrivateEnvironment => {
                let previous = self.top().private_environment_stack.pop().ok_or_else(|| {
                    InterpError::Internal("class private environment stack is unbalanced".into())
                })?;
                self.top().private_brands = previous;
            }

            // ---- arithmetic / binary ----
            Opcode::Add => self.binop(add),
            Opcode::Sub => self.binop(sub),
            Opcode::Mul => self.binop(mul),
            Opcode::Div => self.binop(div),
            Opcode::Mod => self.binop(rem),
            Opcode::Exp => self.binop(pow),
            Opcode::BitAnd => self.binop(bitand),
            Opcode::BitOr => self.binop(bitor),
            Opcode::BitXor => self.binop(bitxor),
            Opcode::Shl => self.binop(shl),
            Opcode::Shr => self.binop(shr),
            Opcode::Ushr => self.binop(ushr),

            // ---- comparison ----
            Opcode::Eq => self.cmp(eq_loose),
            Opcode::StrictEq => self.cmp(eq_strict),
            Opcode::Lt => self.cmp(cmp_lt),
            Opcode::Le => self.cmp(cmp_le),
            Opcode::Gt => self.cmp(cmp_gt),
            Opcode::Ge => self.cmp(cmp_ge),
            Opcode::Instanceof => {
                let b = self.top().stack.pop();
                let a = self.top().stack.pop();
                self.top()
                    .stack
                    .push(Value::boolean(crate::builtins::instanceof_check(&a, &b)));
            }
            Opcode::In => {
                let object = self.top().stack.pop();
                let key = self.top().stack.pop();
                self.ensure_deferred_namespace(modules, &object)?;
                if !is_object_value(&object) {
                    return Err(InterpError::Throw(type_error(
                        "right-hand side of 'in' is not an object",
                    )));
                }
                self.top()
                    .stack
                    .push(Value::boolean(has_property(&object, &key)));
            }

            // ---- unary ----
            Opcode::Neg => self.unary(neg),
            Opcode::Pos => self.unary(pos),
            Opcode::Not => {
                let b = is_falsy(&self.top().stack.pop());
                self.top().stack.push(Value::boolean(b));
            }
            Opcode::BitNot => self.unary(bitnot),
            Opcode::Typeof => self.unary(typeof_),
            Opcode::Void => {
                self.top().stack.pop();
                self.top().stack.push(Value::undefined());
            }

            // ---- globals ----
            Opcode::GetGlobal => {
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str().to_string(),
                    _ => String::new(),
                };
                let (binding, global_object) = {
                    let realm = self.ctx.realm.borrow();
                    (
                        realm.globals.get(&name).cloned(),
                        realm.global_object.clone(),
                    )
                };
                let global = Value::object(global_object);
                let v = binding
                    .or_else(|| {
                        has_property(&global, &Value::string(name.as_str()))
                            .then(|| get_property(&global, &Value::string(name.as_str())))
                    })
                    .ok_or_else(|| {
                        InterpError::Throw(crate::builtins::error_ctor(
                            &Value::undefined(),
                            &[Value::string(format!("{name} is not defined"))],
                            "ReferenceError",
                        ))
                    })?;
                self.top().stack.push(v);
            }
            Opcode::TypeofGlobal => {
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str(),
                    _ => "",
                };
                let (value, global_object) = {
                    let realm = self.ctx.realm.borrow();
                    (
                        realm.globals.get(name).cloned(),
                        realm.global_object.clone(),
                    )
                };
                let value = value.or_else(|| {
                    let global = Value::object(global_object);
                    has_property(&global, &Value::string(name))
                        .then(|| get_property(&global, &Value::string(name)))
                });
                self.top().stack.push(match value {
                    Some(value) => typeof_(value),
                    None => Value::string("undefined"),
                });
            }
            Opcode::SetGlobal => {
                let v = self.top().stack.pop();
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str().to_string(),
                    _ => String::new(),
                };
                self.ctx
                    .realm
                    .borrow_mut()
                    .globals
                    .insert(name.clone(), v.clone());
                let global = Value::object(self.ctx.realm.borrow().global_object.clone());
                set_property(&global, &Value::string(name), v);
            }
            Opcode::EnterWith => {
                let object = self.top().stack.pop();
                if object.is_nullish() {
                    return Err(InterpError::Throw(type_error(
                        "with object cannot be null or undefined",
                    )));
                }
                self.top().with_environments.push(object);
            }
            Opcode::LeaveWith => {
                self.top().with_environments.pop().ok_or_else(|| {
                    InterpError::Internal("unbalanced object Environment Record".into())
                })?;
            }
            Opcode::GetName | Opcode::TypeofName => {
                let name = constant_string(module, ins.operand);
                let value = if let Some(object) = self.with_binding_object(modules, &name)? {
                    self.get_property_value(
                        modules,
                        &object,
                        &Value::string(name.as_str()),
                        &object,
                    )?
                } else {
                    match self.static_name_value(module, &name) {
                        Ok(value) => value,
                        Err(error) if ins.op == Opcode::TypeofName => {
                            let _ = error;
                            Value::undefined()
                        }
                        Err(error) => return Err(error),
                    }
                };
                self.top().stack.push(if ins.op == Opcode::TypeofName {
                    typeof_(value)
                } else {
                    value
                });
            }
            Opcode::SetName => {
                let value = self.top().stack.pop();
                let name = constant_string(module, ins.operand);
                if let Some(object) = self.with_binding_object(modules, &name)? {
                    self.set_property_value(
                        modules,
                        &object,
                        &Value::string(name.as_str()),
                        value,
                        &object,
                    )?;
                } else {
                    self.set_static_name(module, &name, value)?;
                }
            }
            Opcode::DeleteName => {
                let name = constant_string(module, ins.operand);
                let deleted = if let Some(object) = self.with_binding_object(modules, &name)? {
                    delete_property(&object, &Value::string(name.as_str()))
                } else {
                    self.delete_static_name(module, &name)
                };
                self.top().stack.push(Value::boolean(deleted));
            }
            Opcode::DeleteGlobal => {
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str().to_string(),
                    _ => String::new(),
                };
                let removed = self.ctx.realm.borrow_mut().globals.remove(&name).is_some();
                let global = Value::object(self.ctx.realm.borrow().global_object.clone());
                let deleted = delete_property(&global, &Value::string(name.as_str()));
                self.top().stack.push(Value::boolean(removed || deleted));
            }

            // ---- control flow ----
            Opcode::Jump => {
                self.top().pc = ins.operand as usize;
            }
            Opcode::JumpIfTrue => {
                let v = self.top().stack.pop();
                if is_truthy(&v) {
                    self.top().pc = ins.operand as usize;
                }
            }
            Opcode::JumpIfFalse => {
                let v = self.top().stack.pop();
                if is_falsy(&v) {
                    self.top().pc = ins.operand as usize;
                }
            }
            Opcode::JumpIfNullish => {
                let value = self.top().stack.pop();
                if value.is_nullish() {
                    self.top().pc = ins.operand as usize;
                }
            }
            Opcode::Return => {
                let (ret, was_construct, this_obj, gen, async_generator_promise, async_promise) = {
                    let f = self.frames.last_mut().unwrap();
                    (
                        f.stack.pop(),
                        f.is_construct,
                        f.this.clone(),
                        f.generator.clone(),
                        f.async_generator_promise.clone(),
                        f.async_promise.clone(),
                    )
                };
                // A generator body completing: mark done, return {value, done:true}.
                if let Some(g) = gen {
                    g.borrow_mut().done = true;
                    self.frames.pop();
                    if let Some(promise) = async_generator_promise {
                        crate::builtins::fulfill_promise(
                            self,
                            promise.clone(),
                            iter_result(ret, true),
                        );
                        self.finish_async_generator_request(g);
                        if self.frames.is_empty() {
                            return Ok(Step::Done(Value::object(promise)));
                        }
                        self.top().stack.push(Value::object(promise));
                        return Ok(Step::More);
                    }
                    if self.frames.is_empty() {
                        return Ok(Step::Done(Value::undefined()));
                    }
                    self.top().stack.push(iter_result(ret, true));
                    return Ok(Step::More);
                }
                if self.frames.len() == 1 {
                    self.drain_jobs(modules)?;
                }
                self.frames.pop();
                let ret = if was_construct && !ret.is_object() {
                    this_obj
                } else if let Some(promise) = async_promise {
                    crate::builtins::resolve_promise(self, modules, promise.clone(), ret)?;
                    Value::object(promise)
                } else {
                    ret
                };
                if self.frames.is_empty() {
                    return Ok(Step::Done(ret));
                }
                self.top().stack.push(ret);
            }
            Opcode::Throw => {
                let v = self.top().stack.pop();
                return Err(InterpError::Throw(v));
            }
            Opcode::TryBegin => {
                let idx = self.top().func_index;
                let spec = func_ref(module, idx).handlers[ins.operand as usize];
                self.top().try_stack.push(crate::frame::ActiveTry {
                    catch_pc: spec.catch_pc,
                    finally_pc: spec.finally_pc,
                });
            }
            Opcode::TryEnd => {
                self.top().try_stack.pop();
            }
            Opcode::FinallyEnd => {
                // Re-raise the pending exception (try/finally with no catch),
                // else continue normally.
                if let Some(v) = self.top().pending_throw.take() {
                    return Err(InterpError::Throw(v));
                }
            }
            Opcode::Yield => {
                let yielded = self.frames.last_mut().unwrap().stack.pop();
                return self.suspend_generator(modules, yielded, false);
            }
            Opcode::YieldStar => return self.step_yield_star(modules),
            Opcode::Await => {
                let awaited = self.top().stack.pop();
                let awaited = crate::builtins::promise_resolved(self, modules, awaited)?;
                if self.frames.len() == 1 && self.top().func_index == 0 {
                    return Ok(Step::Suspend(awaited));
                }
                let await_id = self.next_async_continuation;
                self.next_async_continuation += 1;
                let frame = self.frames.pop().expect("awaiting frame");
                let result_promise = frame
                    .async_generator_promise
                    .clone()
                    .or_else(|| frame.async_promise.clone())
                    .ok_or_else(|| {
                        InterpError::Internal("await executed outside an async function".into())
                    })?;
                self.suspended_async
                    .insert(await_id, AsyncContinuation { frame });
                crate::builtins::register_await_reaction(self, &awaited, await_id)?;
                if let Some(caller) = self.frames.last_mut() {
                    caller.stack.push(Value::object(result_promise));
                }
            }

            // ---- calls ----
            Opcode::Call => {
                let (callee, args) = pop_args(self, ins.operand);
                self.invoke(modules, callee, args, Value::undefined(), false);
            }
            Opcode::CallWithArgumentList | Opcode::CallDirectEvalWithArgumentList => {
                let arguments = self.top().stack.pop();
                let args = argument_list_values(&arguments)?;
                let callee = self.top().stack.pop();
                let intrinsic_eval = ins.op == Opcode::CallDirectEvalWithArgumentList
                    && callee
                        .as_function()
                        .is_some_and(|function| function.native == Some(crate::builtins::id::EVAL));
                if intrinsic_eval {
                    let input = args.into_iter().next().unwrap_or_else(Value::undefined);
                    let value = self.eval_value(modules, input, true)?;
                    self.top().stack.push(value);
                } else {
                    self.invoke(modules, callee, args, Value::undefined(), false);
                }
            }
            Opcode::CallDirectEval => {
                let (callee, args) = pop_args(self, ins.operand);
                let intrinsic_eval = callee
                    .as_function()
                    .is_some_and(|function| function.native == Some(crate::builtins::id::EVAL));
                if intrinsic_eval {
                    let input = args.into_iter().next().unwrap_or_else(Value::undefined);
                    let value = self.eval_value(modules, input, true)?;
                    self.top().stack.push(value);
                } else {
                    self.invoke(modules, callee, args, Value::undefined(), false);
                }
            }
            Opcode::CallMethod => {
                // Stack: [obj, fn, args...]
                let (args, this, callee) = {
                    let n = ins.operand as usize;
                    let frame = self.frames.last_mut().unwrap();
                    let mut a: Vec<Value> = (0..n).map(|_| frame.stack.pop()).collect();
                    a.reverse();
                    let mut callee = frame.stack.pop();
                    let this = frame.stack.pop();
                    if let (Some(receiver), Some(target)) =
                        (this.as_function(), callee.as_function_mut())
                    {
                        for (&class_id, &brand) in &receiver.private_brands {
                            target.private_brands.insert(class_id, brand);
                        }
                        target.class_field_keys = receiver.class_field_keys.clone();
                        target.superclass = receiver.superclass.clone();
                        if target.name == "<class-static-initializer>" {
                            target.home_object = Some(Box::new(this.clone()));
                        }
                    }
                    (a, this, callee)
                };
                self.invoke(modules, callee, args, this, false);
            }
            Opcode::CallMethodWithArgumentList => {
                let arguments = self.top().stack.pop();
                let args = argument_list_values(&arguments)?;
                let mut callee = self.top().stack.pop();
                let this = self.top().stack.pop();
                inherit_method_context(&this, &mut callee);
                self.invoke(modules, callee, args, this, false);
            }
            Opcode::CallSuper | Opcode::CallSuperWithArgumentList => {
                let args = if ins.op == Opcode::CallSuperWithArgumentList {
                    let arguments = self.top().stack.pop();
                    argument_list_values(&arguments)?
                } else if ins.operand == u16::MAX {
                    self.frames.last().unwrap().arguments.clone()
                } else {
                    let count = ins.operand as usize;
                    let mut args: Vec<_> = (0..count).map(|_| self.top().stack.pop()).collect();
                    args.reverse();
                    args
                };
                let superclass = self
                    .frames
                    .last()
                    .and_then(|frame| frame.superclass.clone())
                    .ok_or_else(|| {
                        InterpError::Internal("super() has no runtime superclass".into())
                    })?;
                let derived_constructor = self
                    .frames
                    .last()
                    .and_then(|frame| frame.constructor.clone());
                let super_base = get_property(&superclass, &Value::string("prototype"));
                let base = self.frames.last().unwrap().this.clone();
                if superclass
                    .as_function()
                    .is_some_and(|function| function.superclass.is_none())
                {
                    self.initialize_instance_elements(modules, &superclass, &base)?;
                }
                let result = self.call_value_mode(modules, superclass, args, base.clone(), true)?;
                if let Some(constructor) = derived_constructor {
                    self.initialize_instance_elements(modules, &constructor, &result)?;
                }
                let frame = self.frames.last_mut().unwrap();
                frame.super_base = Some(super_base);
                frame.this = if result.is_object() {
                    result.clone()
                } else {
                    base
                };
                frame
                    .this_binding
                    .set(frame.this.clone())
                    .map_err(binding_error_value)?;
                frame.stack.push(result);
            }
            Opcode::SetSuperProp => {
                let key = self.top().stack.pop();
                let value = self.top().stack.pop();
                self.set_super_property(modules, &key, value)?;
            }
            Opcode::GetSuperProp => {
                let key = self.top().stack.pop();
                let (base, receiver) = self.super_property_base()?;
                let value = self.get_property_value(modules, &base, &key, &receiver)?;
                self.top().stack.push(value);
            }
            Opcode::NewObject => {
                let o = js_runtime::object::ObjectData::new_handle();
                o.borrow_mut().proto = self.object_prototype();
                self.top().stack.push(Value::object(o));
            }
            Opcode::NewRegex => {
                // Constant holds "pattern\0flags".
                let combined = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str().to_string(),
                    _ => String::new(),
                };
                let (pattern, flags) = combined
                    .split_once('\0')
                    .map(|(p, f)| (p.to_string(), f.to_string()))
                    .unwrap_or((combined.clone(), String::new()));
                let global = flags.contains('g');
                let ignore_case = flags.contains('i');
                let multiline = flags.contains('m');
                let dotall = flags.contains('s');
                let sticky = flags.contains('y');
                let unicode = flags.contains('u');
                let o = js_runtime::object::ObjectData::new_handle();
                {
                    let mut b = o.borrow_mut();
                    b.class = "RegExp";
                    let pd = |v: Value| js_runtime::object::PropertyDescriptor::data(v);
                    b.properties
                        .insert("source".into(), pd(Value::string(pattern)));
                    b.properties
                        .insert("flags".into(), pd(Value::string(flags)));
                    b.properties
                        .insert("global".into(), pd(Value::boolean(global)));
                    b.properties
                        .insert("ignoreCase".into(), pd(Value::boolean(ignore_case)));
                    b.properties
                        .insert("multiline".into(), pd(Value::boolean(multiline)));
                    b.properties
                        .insert("dotAll".into(), pd(Value::boolean(dotall)));
                    b.properties
                        .insert("sticky".into(), pd(Value::boolean(sticky)));
                    b.properties
                        .insert("unicode".into(), pd(Value::boolean(unicode)));
                    b.properties
                        .insert("lastIndex".into(), pd(Value::integer(0)));
                }
                self.top().stack.push(Value::object(o));
            }
            Opcode::NewArray => {
                let n = ins.operand as usize;
                let mut args: Vec<Value> = (0..n).map(|_| self.top().stack.pop()).collect();
                args.reverse();
                let o = js_runtime::object::ObjectData::new_handle();
                {
                    let mut obj = o.borrow_mut();
                    obj.class = "Array";
                    obj.is_exotic_array = true;
                    obj.proto = self.array_prototype();
                    for (i, v) in args.iter().enumerate() {
                        obj.properties.insert(
                            i.to_string(),
                            js_runtime::object::PropertyDescriptor::data(v.clone()),
                        );
                    }
                    obj.properties.insert(
                        "length".to_string(),
                        js_runtime::object::PropertyDescriptor::data(Value::integer(n as i32)),
                    );
                }
                self.top().stack.push(Value::object(o));
            }
            Opcode::GetTemplateObject => {
                let key = (module_index, self.top().func_index, ins.operand);
                let value = if let Some(value) = self.template_objects.get(&key) {
                    value.clone()
                } else {
                    let site = &func_ref(module, self.top().func_index).template_sites
                        [ins.operand as usize];
                    let value = create_template_object(site);
                    self.template_objects.insert(key, value.clone());
                    value
                };
                self.top().stack.push(value);
            }
            Opcode::GetImportMeta => {
                let value = self
                    .import_meta_objects
                    .entry(module_index)
                    .or_insert_with(|| {
                        let object = js_runtime::object::ObjectData::new_handle();
                        object.borrow_mut().explicit_null_prototype = true;
                        Value::object(object)
                    })
                    .clone();
                self.top().stack.push(value);
            }
            Opcode::GetProp => {
                let key = self.top().stack.pop();
                let obj = self.top().stack.pop();
                self.ensure_deferred_namespace(modules, &obj)?;
                let value = self.get_property_value(modules, &obj, &key, &obj)?;
                self.top().stack.push(value);
            }
            Opcode::SetProp => {
                // Stack: [..., value, obj, key] (key on top). Pops key, obj,
                // value; the compiler keeps the assignment result via a
                // preceding `Dup` of the value.
                let key = self.top().stack.pop();
                let obj = self.top().stack.pop();
                let value = self.top().stack.pop();
                self.ensure_deferred_namespace(modules, &obj)?;
                if !self.set_property_value(modules, &obj, &key, value, &obj)? {
                    return Err(InterpError::Throw(type_error("property assignment failed")));
                }
            }
            Opcode::DefineDataProperty | Opcode::DefineMethod => {
                let key = self.top().stack.pop();
                let object = self.top().stack.pop();
                let mut value = self.top().stack.pop();
                let handle = obj_as_object(&object).cloned().ok_or_else(|| {
                    InterpError::Throw(type_error("class element base is not an object"))
                })?;
                if ins.op == Opcode::DefineMethod {
                    if let Some(function) = value.as_function_mut() {
                        function.home_object = Some(Box::new(object.clone()));
                    }
                }
                let descriptor = PropertyDescriptor::Data {
                    value,
                    attr: js_runtime::object::Attribute {
                        writable: true,
                        enumerable: ins.op == Opcode::DefineDataProperty,
                        configurable: true,
                    },
                };
                let proxy = { handle.borrow().proxy.clone() };
                let defined = if let Some(proxy) = proxy {
                    let trap = self.get_property_value(
                        modules,
                        &proxy.handler,
                        &Value::string("defineProperty"),
                        &proxy.handler,
                    )?;
                    if trap.is_undefined() {
                        let target = obj_as_object(&proxy.target).cloned().ok_or_else(|| {
                            InterpError::Internal("Proxy target is not an object".into())
                        })?;
                        define_own_property(&target, &key, descriptor)
                    } else if trap.is_function() {
                        let descriptor_value = property_descriptor_value(&descriptor);
                        is_truthy(&self.call_value(
                            modules,
                            trap,
                            vec![proxy.target, key.clone(), descriptor_value],
                            proxy.handler,
                        )?)
                    } else {
                        return Err(InterpError::Throw(type_error(
                            "Proxy defineProperty trap is not callable",
                        )));
                    }
                } else {
                    define_own_property(&handle, &key, descriptor)
                };
                if !defined {
                    return Err(InterpError::Throw(type_error(
                        "class element property cannot be defined",
                    )));
                }
            }
            Opcode::DefineGetter | Opcode::DefineSetter => {
                let key = self.top().stack.pop();
                let object = self.top().stack.pop();
                let mut function = self.top().stack.pop();
                if let Some(callable) = function.as_function_mut() {
                    callable.home_object = Some(Box::new(object.clone()));
                }
                define_accessor(&object, &key, function, ins.op == Opcode::DefineGetter);
            }
            Opcode::GetPrivate => {
                let object = self.top().stack.pop();
                let private_name = self.private_name(module, ins.operand)?;
                let descriptor = obj_as_object(&object)
                    .and_then(|object| object.borrow().private_elements.get(&private_name).cloned())
                    .ok_or_else(|| {
                        InterpError::Throw(type_error(
                            "private member is not declared on this object",
                        ))
                    })?;
                let value = match descriptor {
                    PropertyDescriptor::Data { value, .. } => value,
                    PropertyDescriptor::Accessor { get, .. } => match get {
                        Some(getter) => {
                            self.call_value(modules, getter, Vec::new(), object.clone())?
                        }
                        None => {
                            return Err(InterpError::Throw(type_error(
                                "private accessor has no getter",
                            )))
                        }
                    },
                };
                self.top().stack.push(value);
            }
            Opcode::SetPrivate => {
                let object = self.top().stack.pop();
                let value = self.top().stack.pop();
                let private_name = self.private_name(module, ins.operand)?;
                let handle = obj_as_object(&object).cloned().ok_or_else(|| {
                    InterpError::Throw(type_error("private member base is not an object"))
                })?;
                let descriptor = handle
                    .borrow()
                    .private_elements
                    .get(&private_name)
                    .cloned()
                    .ok_or_else(|| {
                        InterpError::Throw(type_error(
                            "private member is not declared on this object",
                        ))
                    })?;
                match descriptor {
                    PropertyDescriptor::Data { attr, .. } if attr.writable => {
                        handle
                            .borrow_mut()
                            .private_elements
                            .insert(private_name, PropertyDescriptor::Data { value, attr });
                    }
                    PropertyDescriptor::Accessor {
                        set: Some(setter), ..
                    } => {
                        self.call_value(modules, setter, vec![value], object)?;
                    }
                    _ => {
                        return Err(InterpError::Throw(type_error(
                            "private member is not writable",
                        )))
                    }
                }
            }
            Opcode::DefinePrivate
            | Opcode::DefinePrivateMethod
            | Opcode::DefinePrivateGetter
            | Opcode::DefinePrivateSetter
            | Opcode::DefinePrivateMethodTemplate
            | Opcode::DefinePrivateGetterTemplate
            | Opcode::DefinePrivateSetterTemplate => {
                let object = self.top().stack.pop();
                let mut value = self.top().stack.pop();
                let private_name = self.private_name(module, ins.operand)?;
                if ins.op != Opcode::DefinePrivate {
                    let home_object = if matches!(
                        ins.op,
                        Opcode::DefinePrivateMethodTemplate
                            | Opcode::DefinePrivateGetterTemplate
                            | Opcode::DefinePrivateSetterTemplate
                    ) {
                        get_property(&object, &Value::string("prototype"))
                    } else {
                        object.clone()
                    };
                    if let Some(function) = value.as_function_mut() {
                        function.home_object = Some(Box::new(home_object));
                    }
                }
                define_private_element(&object, private_name, value, ins.op)?;
            }
            Opcode::PrivateIn => {
                let object = self.top().stack.pop();
                let private_name = self.private_name(module, ins.operand)?;
                let handle = obj_as_object(&object).ok_or_else(|| {
                    InterpError::Throw(type_error(
                        "right-hand side of private `in` is not an object",
                    ))
                })?;
                let present = handle.borrow().private_elements.contains_key(&private_name);
                self.top().stack.push(Value::boolean(present));
            }
            Opcode::CopyDataProperties => {
                let source = self.top().stack.pop();
                let target = self.top().stack.pop();
                copy_data_properties(&target, &source);
                self.top().stack.push(target);
            }
            Opcode::DeleteProp => {
                let key = self.top().stack.pop();
                let object = self.top().stack.pop();
                self.ensure_deferred_namespace(modules, &object)?;
                let deleted = delete_property(&object, &key);
                if !deleted && module.is_module {
                    return Err(InterpError::Throw(type_error(
                        "cannot delete a non-configurable property",
                    )));
                }
                self.top().stack.push(Value::boolean(deleted));
            }
            Opcode::DeleteSuperProp => {
                let _key = self.top().stack.pop();
                let _ = self.super_property_base()?;
                return Err(InterpError::Throw(crate::builtins::error_ctor(
                    &Value::undefined(),
                    &[Value::string("cannot delete a super property")],
                    "ReferenceError",
                )));
            }
            Opcode::New | Opcode::NewWithArgumentList => {
                let (callee, args) = if ins.op == Opcode::NewWithArgumentList {
                    let arguments = self.top().stack.pop();
                    let args = argument_list_values(&arguments)?;
                    let callee = self.top().stack.pop();
                    (callee, args)
                } else {
                    pop_args(self, ins.operand)
                };
                if let Some(value) =
                    crate::builtins::construct_builtin(&callee, &args, self.array_prototype())
                {
                    set_constructor_chain(&value, &callee);
                    self.top().stack.push(value);
                    return Ok(Step::More);
                }
                // Construct a fresh object with the constructor's prototype.
                let instance = js_runtime::object::ObjectData::new_handle();
                if let Some(function) = callee.as_function() {
                    let function_object = function.object.borrow();
                    instance.borrow_mut().proto = function_object
                        .properties
                        .get("prototype")
                        .and_then(|descriptor| match descriptor {
                            PropertyDescriptor::Data { value, .. } if value.is_object() => {
                                Some(value.clone())
                            }
                            _ => None,
                        });
                }
                let this = Value::object(instance);
                if callee
                    .as_function()
                    .is_some_and(|function| function.superclass.is_none())
                {
                    self.initialize_instance_elements(modules, &callee, &this)?;
                }
                set_constructor_chain(&this, &callee);
                self.invoke(modules, callee, args, this, true);
            }
            Opcode::DynamicImport => {
                // The source expression was evaluated for side effects; its
                // value (the runtime specifier) is popped only to keep the stack
                // balanced. Resolution uses the request INDEX carried by the
                // opcode, which encodes the full ModuleRequest (specifier +
                // phase + attributes) — so two imports of the same specifier
                // with different attributes resolve to distinct records. A
                // sentinel index (`u16::MAX`, non-literal source) or an index
                // with no host resolution yields "unresolved".
                let _specifier = to_string(&self.top().stack.pop());
                let request_index = ins.operand as usize;
                let module_index = self.top().module_index;
                let resolution = self
                    .dynamic_imports
                    .as_ref()
                    .and_then(|modules| modules.get(module_index))
                    .and_then(|requests| requests.get(request_index))
                    .cloned()
                    .unwrap_or_else(|| {
                        Err("dynamic import was not resolved by the module host".to_string())
                    });
                let promise = crate::builtins::promise_pending();
                self.dynamic_import_requests
                    .push_back(DynamicImportRequest {
                        resolution,
                        promise: promise.clone(),
                    });
                self.top().stack.push(Value::object(promise));
            }

            Opcode::LogicalAnd | Opcode::LogicalOr | Opcode::NullishCoal => {
                return Err(InterpError::Internal(format!(
                    "opcode {:?} not implemented yet (lowered to runtime calls)",
                    ins.op
                )));
            }
        }
        Ok(Step::More)
    }

    /// Short-hand for the currently executing frame.
    fn top(&mut self) -> &mut CallFrame {
        self.frames.last_mut().unwrap()
    }

    fn complete_generator_request(&mut self, value: Value) -> Result<Step, InterpError> {
        let (generator, promise) = {
            let frame = self.frames.last().expect("generator frame");
            (
                frame.generator.clone().expect("generator frame owner"),
                frame.async_generator_promise.clone(),
            )
        };
        {
            let mut state = generator.borrow_mut();
            state.done = true;
            state.delegate = None;
        }
        self.frames.pop();
        if let Some(promise) = promise {
            crate::builtins::fulfill_promise(self, promise.clone(), iter_result(value, true));
            self.finish_async_generator_request(generator);
            let result = Value::object(promise);
            if self.frames.is_empty() {
                Ok(Step::Done(result))
            } else {
                self.top().stack.push(result);
                Ok(Step::More)
            }
        } else if self.frames.is_empty() {
            Ok(Step::Done(Value::undefined()))
        } else {
            self.top().stack.push(iter_result(value, true));
            Ok(Step::More)
        }
    }

    fn suspend_generator(
        &mut self,
        modules: &BytecodeGraph<'_>,
        yielded: Value,
        repeat_current_instruction: bool,
    ) -> Result<Step, InterpError> {
        let (
            generator,
            promise,
            pc,
            locals,
            stack,
            upvalues,
            with_environments,
            private_brands,
            private_environment_stack,
            this,
            captured_this,
            try_stack,
            pending_throw,
        ) = {
            let frame = self.frames.last_mut().expect("generator frame");
            let generator = frame.generator.clone().expect("generator frame owner");
            if repeat_current_instruction {
                frame.pc = frame.pc.saturating_sub(1);
            }
            let depth = frame.stack.depth();
            let mut stack: Vec<Value> = (0..depth).map(|_| frame.stack.pop()).collect();
            stack.reverse();
            (
                generator,
                frame.async_generator_promise.clone(),
                frame.pc,
                std::mem::take(&mut frame.locals),
                stack,
                std::mem::take(&mut frame.upvalues),
                std::mem::take(&mut frame.with_environments),
                std::mem::take(&mut frame.private_brands),
                std::mem::take(&mut frame.private_environment_stack),
                frame.this.clone(),
                frame.captured_this.take(),
                std::mem::take(&mut frame.try_stack),
                frame.pending_throw.take(),
            )
        };
        {
            let mut state = generator.borrow_mut();
            state.pc = pc;
            state.locals = locals;
            state.stack = stack;
            state.upvalues = upvalues;
            state.with_environments = with_environments;
            state.private_brands = private_brands;
            state.private_environment_stack = private_environment_stack;
            state.this = this;
            state.captured_this = captured_this;
            state.try_stack = try_stack
                .into_iter()
                .map(|handler| GeneratorTryState {
                    catch_pc: handler.catch_pc,
                    finally_pc: handler.finally_pc,
                })
                .collect();
            state.pending_throw = pending_throw;
            state.done = false;
        }
        self.frames.pop();
        if let Some(promise) = promise {
            match self.await_value_now(modules, yielded) {
                Ok(value) => crate::builtins::fulfill_promise(
                    self,
                    promise.clone(),
                    iter_result(value, false),
                ),
                Err(InterpError::Throw(reason)) => {
                    let mut state = generator.borrow_mut();
                    state.done = true;
                    state.delegate = None;
                    drop(state);
                    crate::builtins::reject_promise(self, promise.clone(), reason);
                }
                Err(error) => return Err(error),
            }
            self.finish_async_generator_request(generator);
            if let Some(caller) = self.frames.last_mut() {
                caller.stack.push(Value::object(promise));
                return Ok(Step::More);
            }
            return Ok(Step::Done(Value::undefined()));
        } else {
            if self.frames.is_empty() {
                return Ok(Step::Done(Value::undefined()));
            }
            self.top().stack.push(iter_result(yielded, false));
        }
        Ok(Step::More)
    }

    fn await_value_now(
        &mut self,
        modules: &BytecodeGraph<'_>,
        value: Value,
    ) -> Result<Value, InterpError> {
        let promise = crate::builtins::promise_resolved(self, modules, value)?;
        self.drain_jobs(modules)?;
        match crate::builtins::promise_result(&promise) {
            Some(crate::builtins::AwaitedPromise::Fulfilled(value)) => Ok(value),
            Some(crate::builtins::AwaitedPromise::Rejected(reason)) => {
                Err(InterpError::Throw(reason))
            }
            Some(crate::builtins::AwaitedPromise::Pending) => Err(InterpError::Internal(
                "yield* awaited a pending Promise without a host continuation".into(),
            )),
            None => unreachable!("PromiseResolve always returns a Promise"),
        }
    }

    fn step_yield_star(&mut self, modules: &BytecodeGraph<'_>) -> Result<Step, InterpError> {
        let generator = self
            .frames
            .last()
            .and_then(|frame| frame.generator.clone())
            .expect("`yield*` outside a generator frame");
        let is_async = generator.borrow().is_async;

        if generator.borrow().delegate.is_none() {
            let iterable = self.top().stack.pop();
            let delegate = self.get_iterator_record(modules, iterable, is_async)?;
            generator.borrow_mut().delegate = Some(delegate);
        }

        let (kind, mut received) = self
            .top()
            .generator_resume
            .take()
            .unwrap_or((GeneratorResumeKind::Next, Value::undefined()));
        if is_async && kind == GeneratorResumeKind::Return {
            received = self.await_value_now(modules, received)?;
        }

        let delegate = generator
            .borrow()
            .delegate
            .clone()
            .expect("yield* iterator record");
        let result = match kind {
            GeneratorResumeKind::Next if delegate.intrinsic_next => {
                step_array_iterator(&delegate.iterator)
            }
            GeneratorResumeKind::Next => self.call_iterator_method(
                modules,
                &delegate.iterator,
                delegate.next_method.clone(),
                received,
            )?,
            GeneratorResumeKind::Throw => {
                let method =
                    self.get_optional_method(modules, &delegate.iterator, Value::string("throw"))?;
                let Some(method) = method else {
                    if let Some(return_method) = self.get_optional_method(
                        modules,
                        &delegate.iterator,
                        Value::string("return"),
                    )? {
                        let close_result = self.call_value(
                            modules,
                            return_method,
                            Vec::new(),
                            delegate.iterator.clone(),
                        )?;
                        let close_result = if is_async {
                            self.await_value_now(modules, close_result)?
                        } else {
                            close_result
                        };
                        if !close_result.is_object() {
                            return Err(InterpError::Throw(type_error(
                                "iterator return result is not an object",
                            )));
                        }
                    }
                    return Err(InterpError::Throw(type_error(
                        "iterator does not provide a throw method",
                    )));
                };
                self.call_iterator_method(modules, &delegate.iterator, method, received)?
            }
            GeneratorResumeKind::Return => {
                let method =
                    self.get_optional_method(modules, &delegate.iterator, Value::string("return"))?;
                let Some(method) = method else {
                    generator.borrow_mut().delegate = None;
                    return self.complete_generator_request(received);
                };
                self.call_iterator_method(modules, &delegate.iterator, method, received)?
            }
        };

        let result = if is_async && !delegate.async_from_sync {
            self.await_value_now(modules, result)?
        } else {
            result
        };
        if !result.is_object() {
            return Err(InterpError::Throw(type_error(
                "iterator result is not an object",
            )));
        }
        let done = self.get_property_value(modules, &result, &Value::string("done"), &result)?;
        let done = is_truthy(&done);
        let mut value =
            self.get_property_value(modules, &result, &Value::string("value"), &result)?;
        if is_async && delegate.async_from_sync {
            value = self.await_value_now(modules, value)?;
        }

        if done {
            generator.borrow_mut().delegate = None;
            if kind == GeneratorResumeKind::Return {
                self.complete_generator_request(value)
            } else {
                self.top().stack.push(value);
                Ok(Step::More)
            }
        } else {
            self.suspend_generator(modules, value, true)
        }
    }

    fn get_iterator_record(
        &mut self,
        modules: &BytecodeGraph<'_>,
        iterable: Value,
        is_async: bool,
    ) -> Result<IteratorRecord, InterpError> {
        let indexable = matches!(
            iterable.data(),
            ValueData::Object(object) if object.borrow().is_exotic_array
        ) || matches!(iterable.data(), ValueData::String(_));
        if indexable {
            return Ok(IteratorRecord {
                iterator: make_iterator(&iterable),
                next_method: Value::undefined(),
                done: false,
                async_from_sync: is_async,
                intrinsic_next: true,
            });
        }

        if let ValueData::Generator(inner) = iterable.data() {
            let inner_is_async = inner.borrow().is_async;
            if !is_async && inner_is_async {
                return Err(InterpError::Throw(type_error(
                    "value is not synchronously iterable",
                )));
            }
            let iterator = iterable.clone();
            let next_method =
                self.get_property_value(modules, &iterator, &Value::string("next"), &iterator)?;
            return Ok(IteratorRecord {
                iterator,
                next_method,
                done: false,
                async_from_sync: is_async && !inner_is_async,
                intrinsic_next: false,
            });
        }

        let (method, async_from_sync) = if is_async {
            match self.get_optional_method(
                modules,
                &iterable,
                Value::symbol(js_runtime::value::JsSymbol::async_iterator()),
            )? {
                Some(method) => (method, false),
                None => (
                    self.get_required_method(
                        modules,
                        &iterable,
                        Value::symbol(js_runtime::value::JsSymbol::iterator()),
                    )?,
                    true,
                ),
            }
        } else {
            (
                self.get_required_method(
                    modules,
                    &iterable,
                    Value::symbol(js_runtime::value::JsSymbol::iterator()),
                )?,
                false,
            )
        };
        let iterator = self.call_value(modules, method, Vec::new(), iterable)?;
        if !iterator.is_object() {
            return Err(InterpError::Throw(type_error(
                "iterator method returned a non-object",
            )));
        }
        let next_method =
            self.get_property_value(modules, &iterator, &Value::string("next"), &iterator)?;
        Ok(IteratorRecord {
            iterator,
            next_method,
            done: false,
            async_from_sync,
            intrinsic_next: false,
        })
    }

    fn get_required_method(
        &mut self,
        modules: &BytecodeGraph<'_>,
        object: &Value,
        key: Value,
    ) -> Result<Value, InterpError> {
        self.get_optional_method(modules, object, key)?
            .ok_or_else(|| InterpError::Throw(type_error("value is not iterable")))
    }

    fn get_optional_method(
        &mut self,
        modules: &BytecodeGraph<'_>,
        object: &Value,
        key: Value,
    ) -> Result<Option<Value>, InterpError> {
        let method = self.get_property_value(modules, object, &key, object)?;
        if method.is_nullish() {
            Ok(None)
        } else if method.is_function() {
            Ok(Some(method))
        } else {
            Err(InterpError::Throw(type_error(
                "iterator method is not callable",
            )))
        }
    }

    fn call_iterator_method(
        &mut self,
        modules: &BytecodeGraph<'_>,
        iterator: &Value,
        method: Value,
        argument: Value,
    ) -> Result<Value, InterpError> {
        if !method.is_function() {
            return Err(InterpError::Throw(type_error(
                "iterator method is not callable",
            )));
        }
        self.call_value(modules, method, vec![argument], iterator.clone())
    }

    pub(crate) fn get_property_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        object: &Value,
        key: &Value,
        receiver: &Value,
    ) -> Result<Value, InterpError> {
        if let Some(proxy) = obj_as_object(object).and_then(|object| object.borrow().proxy.clone())
        {
            let trap = self.get_property_value(
                modules,
                &proxy.handler,
                &Value::string("get"),
                &proxy.handler,
            )?;
            if trap.is_undefined() {
                return self.get_property_value(modules, &proxy.target, key, receiver);
            }
            if !trap.is_function() {
                return Err(InterpError::Throw(type_error(
                    "Proxy get trap is not callable",
                )));
            }
            return self.call_value(
                modules,
                trap,
                vec![proxy.target, key.clone(), receiver.clone()],
                proxy.handler,
            );
        }
        if let ValueData::String(name) = key.data() {
            if let Some(value) = crate::builtins::native_static_value(object, &name.0) {
                return Ok(value);
            }
        }
        let Some(handle) = obj_as_object(object) else {
            return get_property_checked(object, key).map_err(binding_error_value);
        };
        let mut current = Some(handle.clone());
        while let Some(candidate) = current {
            let (descriptor, prototype) = {
                let data = candidate.borrow();
                if let ValueData::Symbol(symbol) = key.data() {
                    let descriptor = data.symbol_properties.get(&symbol.id).cloned();
                    let prototype = data.proto.clone();
                    drop(data);
                    if let Some(descriptor) = descriptor {
                        return match descriptor {
                            js_runtime::object::PropertyDescriptor::Data { value, .. } => Ok(value),
                            js_runtime::object::PropertyDescriptor::Accessor { get, .. } => {
                                match get {
                                    Some(getter) if getter.is_function() => self.call_value(
                                        modules,
                                        getter,
                                        Vec::new(),
                                        receiver.clone(),
                                    ),
                                    _ => Ok(Value::undefined()),
                                }
                            }
                        };
                    }
                    current = prototype.as_ref().and_then(obj_as_object).cloned();
                    continue;
                }
                let name = prop_name(key);
                if let Some(namespace) = &data.module_namespace {
                    return namespace
                        .get(&name)
                        .map_or_else(|| Ok(Value::undefined()), |cell| cell.get())
                        .map_err(binding_error_value);
                }
                (data.properties.get(&name).cloned(), data.proto.clone())
            };
            if let Some(descriptor) = descriptor {
                return match descriptor {
                    js_runtime::object::PropertyDescriptor::Data { value, .. } => Ok(value),
                    js_runtime::object::PropertyDescriptor::Accessor { get, .. } => match get {
                        Some(getter) if getter.is_function() => {
                            self.call_value(modules, getter, Vec::new(), receiver.clone())
                        }
                        _ => Ok(Value::undefined()),
                    },
                };
            }
            current = prototype.as_ref().and_then(obj_as_object).cloned();
        }
        get_property_checked(object, key).map_err(binding_error_value)
    }

    fn has_property_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<bool, InterpError> {
        if let Some(proxy) = obj_as_object(object).and_then(|object| object.borrow().proxy.clone())
        {
            let trap = self.get_property_value(
                modules,
                &proxy.handler,
                &Value::string("has"),
                &proxy.handler,
            )?;
            if trap.is_undefined() {
                return self.has_property_value(modules, &proxy.target, key);
            }
            if !trap.is_function() {
                return Err(InterpError::Throw(type_error(
                    "Proxy has trap is not callable",
                )));
            }
            return self
                .call_value(
                    modules,
                    trap,
                    vec![proxy.target, key.clone()],
                    proxy.handler,
                )
                .map(|result| result.to_boolean());
        }
        Ok(has_property(object, key))
    }

    /// Object Environment Record `HasBinding`, including @@unscopables.
    fn with_binding_object(
        &mut self,
        modules: &BytecodeGraph<'_>,
        name: &str,
    ) -> Result<Option<Value>, InterpError> {
        let environments = self.top().with_environments.clone();
        let name_key = Value::string(name);
        for object in environments.into_iter().rev() {
            if !self.has_property_value(modules, &object, &name_key)? {
                continue;
            }
            let unscopables_key = Value::symbol(js_runtime::value::JsSymbol::unscopables());
            let unscopables =
                self.get_property_value(modules, &object, &unscopables_key, &object)?;
            if is_object_value(&unscopables) {
                let blocked =
                    self.get_property_value(modules, &unscopables, &name_key, &unscopables)?;
                if blocked.to_boolean() {
                    continue;
                }
            }
            return Ok(Some(object));
        }
        Ok(None)
    }

    fn static_name_value(&self, module: &BytecodeModule, name: &str) -> Result<Value, InterpError> {
        let frame = self.frames.last().expect("active frame");
        let function = func_ref(module, frame.func_index);
        if let Some(slot) = function.locals.get(name) {
            return frame.locals[slot as usize]
                .get()
                .map_err(binding_error_value);
        }
        if let Some(index) = function
            .upvalue_names
            .iter()
            .position(|candidate| candidate == name)
        {
            return frame.upvalues[index].get().map_err(binding_error_value);
        }
        let global_binding = self.ctx.realm.borrow().globals.get(name).cloned();
        let global_binding = global_binding.or_else(|| {
            let global = Value::object(self.ctx.realm.borrow().global_object.clone());
            has_property(&global, &Value::string(name))
                .then(|| get_property(&global, &Value::string(name)))
        });
        global_binding.ok_or_else(|| {
            InterpError::Throw(crate::builtins::error_ctor(
                &Value::undefined(),
                &[Value::string(format!("{name} is not defined"))],
                "ReferenceError",
            ))
        })
    }

    fn set_static_name(
        &mut self,
        module: &BytecodeModule,
        name: &str,
        value: Value,
    ) -> Result<(), InterpError> {
        let (local, upvalue) = {
            let frame = self.frames.last().expect("active frame");
            let function = func_ref(module, frame.func_index);
            (
                function.locals.get(name),
                function
                    .upvalue_names
                    .iter()
                    .position(|candidate| candidate == name),
            )
        };
        if let Some(slot) = local {
            return self.top().locals[slot as usize]
                .set(value)
                .map_err(binding_error_value);
        }
        if let Some(index) = upvalue {
            return self.top().upvalues[index]
                .set(value)
                .map_err(binding_error_value);
        }
        self.ctx
            .realm
            .borrow_mut()
            .globals
            .insert(name.to_string(), value.clone());
        let global = Value::object(self.ctx.realm.borrow().global_object.clone());
        set_property(&global, &Value::string(name), value);
        Ok(())
    }

    fn delete_static_name(&mut self, module: &BytecodeModule, name: &str) -> bool {
        let frame = self.frames.last().expect("active frame");
        let function = func_ref(module, frame.func_index);
        if function.locals.get(name).is_some()
            || function
                .upvalue_names
                .iter()
                .any(|candidate| candidate == name)
        {
            return false;
        }
        self.ctx.realm.borrow_mut().globals.remove(name);
        let global = Value::object(self.ctx.realm.borrow().global_object.clone());
        delete_property(&global, &Value::string(name))
    }

    fn set_property_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        object: &Value,
        key: &Value,
        value: Value,
        receiver: &Value,
    ) -> Result<bool, InterpError> {
        if let Some(proxy) = obj_as_object(object).and_then(|object| object.borrow().proxy.clone())
        {
            let trap = self.get_property_value(
                modules,
                &proxy.handler,
                &Value::string("set"),
                &proxy.handler,
            )?;
            if trap.is_undefined() {
                return self.set_property_value(modules, &proxy.target, key, value, receiver);
            }
            if !trap.is_function() {
                return Err(InterpError::Throw(type_error(
                    "Proxy set trap is not callable",
                )));
            }
            return self
                .call_value(
                    modules,
                    trap,
                    vec![proxy.target, key.clone(), value, receiver.clone()],
                    proxy.handler,
                )
                .map(|result| is_truthy(&result));
        }
        let Some(handle) = obj_as_object(object).cloned() else {
            return Ok(set_property_checked(receiver, key, value));
        };
        let name = prop_name(key);
        let mut current = Some(handle);
        while let Some(candidate) = current {
            let (descriptor, prototype, namespace) = {
                let data = candidate.borrow();
                (
                    data.properties.get(&name).cloned(),
                    data.proto.clone(),
                    data.module_namespace.is_some(),
                )
            };
            if namespace {
                return Ok(false);
            }
            if let Some(descriptor) = descriptor {
                return match descriptor {
                    PropertyDescriptor::Accessor {
                        set: Some(setter), ..
                    } => {
                        self.call_value(modules, setter, vec![value], receiver.clone())?;
                        Ok(true)
                    }
                    PropertyDescriptor::Accessor { set: None, .. } => Ok(false),
                    PropertyDescriptor::Data { attr, .. } if !attr.writable => Ok(false),
                    PropertyDescriptor::Data { .. } => {
                        Ok(set_property_checked(receiver, key, value))
                    }
                };
            }
            current = prototype.as_ref().and_then(obj_as_object).cloned();
        }
        Ok(set_property_checked(receiver, key, value))
    }

    fn initialize_instance_elements(
        &mut self,
        modules: &BytecodeGraph<'_>,
        constructor: &Value,
        instance: &Value,
    ) -> Result<(), InterpError> {
        let function = constructor.as_function().ok_or_else(|| {
            InterpError::Internal("instance element owner is not a constructor".into())
        })?;
        let templates = function.object.borrow().private_instance_elements.clone();
        let initializer = function.instance_initializer.as_deref().cloned();
        let instance_object = obj_as_object(instance)
            .ok_or_else(|| InterpError::Throw(type_error("constructed value is not an object")))?;
        install_private_elements(instance_object, &templates)?;
        if let Some(initializer) = initializer {
            self.call_value(modules, initializer, Vec::new(), instance.clone())?;
        }
        Ok(())
    }

    /// Call a JS `callee` with `args`/`this` and run it to completion
    /// synchronously (used by native builtins that take callbacks, e.g.
    /// `Array.map`). Runs a sub-dispatch loop until the callee's frame returns,
    /// then pops and returns its return value.
    pub(crate) fn call_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        callee: Value,
        args: Vec<Value>,
        this: Value,
    ) -> Result<Value, InterpError> {
        self.call_value_mode(modules, callee, args, this, false)
    }

    fn call_value_mode(
        &mut self,
        modules: &BytecodeGraph<'_>,
        callee: Value,
        args: Vec<Value>,
        this: Value,
        is_construct: bool,
    ) -> Result<Value, InterpError> {
        let host_frame = self.frames.is_empty();
        if host_frame {
            let module_index = callee
                .as_function()
                .map(|function| function.module_index as usize)
                .unwrap_or(0);
            let module_ptr = self.module_ptr(modules, module_index).ok_or_else(|| {
                InterpError::Internal(format!(
                    "PromiseJob callback refers to missing module {module_index}"
                ))
            })?;
            let module = unsafe { &*module_ptr };
            self.frames
                .push(CallFrame::for_module(module_index, 0, 0, module.main.span));
        }
        let target = self.frames.len();
        self.invoke(modules, callee, args, this, is_construct);
        // `invoke` may have pushed a frame (bytecode callee) or already pushed
        // a result (native/done-generator). Step until the frame unwinds.
        while self.frames.len() > target {
            match self.step(modules) {
                Ok(Step::More) => {}
                Ok(Step::Done(_)) => break,
                Ok(Step::Suspend(_)) => unreachable!("nested module execution cannot suspend"),
                Err(e) => match self.handle_exception(modules, e, target) {
                    Ok(()) => {} // caught inside the callee
                    Err(e) => {
                        if host_frame {
                            self.frames.clear();
                        }
                        return Err(e);
                    } // escaped the callee → propagate
                },
            }
        }
        // The callee's return value now sits on top of the caller's stack.
        let value = self.top().stack.pop();
        if host_frame {
            self.frames.pop();
        }
        Ok(value)
    }

    fn set_super_property(
        &mut self,
        modules: &BytecodeGraph<'_>,
        key: &Value,
        value: Value,
    ) -> Result<(), InterpError> {
        let (base, receiver) = self.super_property_base()?;
        let name = prop_name(key);
        let setter = obj_as_object(&base).and_then(|object| {
            object
                .borrow()
                .properties
                .get(&name)
                .and_then(|descriptor| match descriptor {
                    PropertyDescriptor::Accessor { set, .. } => set.clone(),
                    _ => None,
                })
        });
        if let Some(setter) = setter {
            self.call_value(modules, setter, vec![value], receiver)?;
            return Ok(());
        }
        if let ValueData::Object(object) = receiver.data() {
            if let Some(namespace) = &object.borrow().module_namespace {
                if let Some(binding) = namespace.get(&name) {
                    binding.get().map_err(binding_error_value)?;
                }
                return Err(InterpError::Throw(type_error(
                    "cannot assign through super to a module namespace",
                )));
            }
        }
        if set_property_checked(&receiver, key, value) {
            Ok(())
        } else {
            Err(InterpError::Throw(type_error(
                "super property assignment failed",
            )))
        }
    }

    fn super_property_base(&self) -> Result<(Value, Value), InterpError> {
        let frame = self.frames.last().unwrap();
        let receiver = self.current_this()?;
        let base = if let Some(home_object) = &frame.home_object {
            obj_as_object(home_object)
                .and_then(|object| object.borrow().proto.clone())
                .unwrap_or_else(Value::null)
        } else if let Some(base) = &frame.super_base {
            base.clone()
        } else if let Some(superclass) = &frame.superclass {
            get_property(superclass, &Value::string("prototype"))
        } else {
            return Err(InterpError::Throw(type_error(
                "super property access has no superclass",
            )));
        };
        Ok((base, receiver))
    }

    pub(crate) fn enqueue_promise_job(&mut self, job: PromiseJob) {
        self.jobs.push_back(job);
    }

    fn finish_async_generator_request(&mut self, generator: Rc<RefCell<GeneratorState>>) {
        let request = {
            let mut state = generator.borrow_mut();
            state.async_executing = false;
            let request = state.async_queue.pop_front();
            if request.is_some() {
                state.async_executing = true;
            }
            request
        };
        if let Some(request) = request {
            self.jobs
                .push_back(PromiseJob::AsyncGeneratorRequest { generator, request });
        }
    }

    fn drain_jobs(&mut self, modules: &BytecodeGraph<'_>) -> Result<(), InterpError> {
        while let Some(job) = self.jobs.pop_front() {
            match job {
                PromiseJob::Reaction {
                    reaction,
                    argument,
                    rejected,
                } => {
                    if let Some(await_id) = reaction.await_id {
                        self.resume_async_continuation(modules, await_id, argument, rejected)?;
                        continue;
                    }
                    let handler = if rejected {
                        reaction.on_rejected.clone()
                    } else {
                        reaction.on_fulfilled.clone()
                    };
                    let outcome = match handler {
                        Some(handler) => self
                            .call_value(
                                modules,
                                handler,
                                vec![argument.clone()],
                                Value::undefined(),
                            )
                            .map(|value| (false, value)),
                        None => Ok((rejected, argument)),
                    };
                    match outcome {
                        Ok((true, value)) => {
                            crate::builtins::reject_promise(self, reaction.result, value)
                        }
                        Ok((false, value)) => {
                            crate::builtins::resolve_promise(self, modules, reaction.result, value)?
                        }
                        Err(InterpError::Throw(value)) => {
                            crate::builtins::reject_promise(self, reaction.result, value)
                        }
                        Err(error) => return Err(error),
                    }
                }
                PromiseJob::ResolveThenable {
                    promise,
                    thenable,
                    then,
                } => {
                    let (resolve, reject) = crate::builtins::resolving_functions(&promise);
                    if let Err(error) =
                        self.call_value(modules, then, vec![resolve, reject], thenable)
                    {
                        match error {
                            InterpError::Throw(reason) => {
                                crate::builtins::reject_promise(self, promise, reason)
                            }
                            error => return Err(error),
                        }
                    }
                }
                PromiseJob::AsyncGeneratorRequest { generator, request } => {
                    if generator.borrow().done {
                        match request.kind {
                            GeneratorResumeKind::Throw => crate::builtins::reject_promise(
                                self,
                                request.promise.clone(),
                                request.value,
                            ),
                            GeneratorResumeKind::Return => crate::builtins::fulfill_promise(
                                self,
                                request.promise.clone(),
                                iter_result(request.value, true),
                            ),
                            GeneratorResumeKind::Next => crate::builtins::fulfill_promise(
                                self,
                                request.promise.clone(),
                                iter_result(Value::undefined(), true),
                            ),
                        }
                        self.finish_async_generator_request(generator);
                        continue;
                    }
                    let target_depth = self.frames.len();
                    let caller_stack_depth = self.frames.last().map(|frame| frame.stack.depth());
                    self.checkout_generator(generator.clone(), request.kind, request.value);
                    self.top().async_generator_promise = Some(request.promise);
                    while self.frames.len() > target_depth {
                        match self.step(modules) {
                            Ok(Step::More) => {}
                            Ok(Step::Done(_)) => break,
                            Ok(Step::Suspend(_)) => {
                                return Err(InterpError::Internal(
                                    "async generator request entered module suspension".into(),
                                ))
                            }
                            Err(error) => self.handle_exception(modules, error, target_depth)?,
                        }
                    }
                    if let (Some(depth), Some(caller)) =
                        (caller_stack_depth, self.frames.last_mut())
                    {
                        while caller.stack.depth() > depth {
                            caller.stack.pop();
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn resume_async_continuation(
        &mut self,
        modules: &BytecodeGraph<'_>,
        await_id: u64,
        argument: Value,
        rejected: bool,
    ) -> Result<(), InterpError> {
        let continuation = self.suspended_async.remove(&await_id).ok_or_else(|| {
            InterpError::Internal(format!("missing async continuation {await_id}"))
        })?;
        let target_depth = self.frames.len();
        let caller_stack_depth = self.frames.last().map(|frame| frame.stack.depth());
        let mut frame = continuation.frame;
        if !rejected {
            frame.stack.push(argument.clone());
        }
        self.frames.push(frame);
        if rejected {
            self.handle_exception(modules, InterpError::Throw(argument), target_depth)?;
        }
        while self.frames.len() > target_depth {
            match self.step(modules) {
                Ok(Step::More) => {}
                Ok(Step::Done(_)) => break,
                Ok(Step::Suspend(_)) => {
                    return Err(InterpError::Internal(
                        "ordinary async continuation entered module suspension".into(),
                    ))
                }
                Err(error) => self.handle_exception(modules, error, target_depth)?,
            }
        }
        if let (Some(depth), Some(caller)) = (caller_stack_depth, self.frames.last_mut()) {
            while caller.stack.depth() > depth {
                caller.stack.pop();
            }
        }
        Ok(())
    }

    pub(crate) fn ensure_deferred_namespace(
        &mut self,
        modules: &BytecodeGraph<'_>,
        value: &Value,
    ) -> Result<(), InterpError> {
        let module_index = match value.data() {
            ValueData::Object(object) => object.borrow().deferred_module,
            _ => None,
        };
        if let Some(module_index) = module_index {
            self.ensure_deferred_module(modules, module_index)?;
        }
        Ok(())
    }

    fn ensure_deferred_module(
        &mut self,
        modules: &BytecodeGraph<'_>,
        module_index: usize,
    ) -> Result<(), InterpError> {
        let (dependencies, locals) = {
            let graph = self.deferred_modules.as_mut().ok_or_else(|| {
                InterpError::Internal("deferred namespace has no registered module graph".into())
            })?;
            let evaluated = graph.evaluated.get_mut(module_index).ok_or_else(|| {
                InterpError::Internal(format!("missing deferred module {module_index}"))
            })?;
            if *evaluated {
                return Ok(());
            }
            // Mark before descending so cycles terminate.
            *evaluated = true;
            (
                graph.dependencies[module_index].clone(),
                graph.locals[module_index].clone(),
            )
        };
        for dependency in dependencies {
            self.ensure_deferred_module(modules, dependency)?;
        }
        let module = modules.get(module_index).copied().ok_or_else(|| {
            InterpError::Internal(format!(
                "missing bytecode for deferred module {module_index}"
            ))
        })?;
        let target_depth = self.frames.len();
        self.frames.push(CallFrame::with_locals(
            module_index,
            0,
            locals,
            module.main.span,
        ));
        while self.frames.len() > target_depth {
            match self.step(modules) {
                Ok(Step::More) => {}
                Ok(Step::Done(_)) => break,
                Ok(Step::Suspend(_)) => unreachable!("nested module execution cannot suspend"),
                Err(error) => match self.handle_exception(modules, error, target_depth) {
                    Ok(()) => {}
                    Err(error) => return Err(error),
                },
            }
        }
        // A nested module completion is delivered to the observing frame.
        if let Some(frame) = self.frames.last_mut() {
            let _ = frame.stack.pop();
        }
        Ok(())
    }

    /// Pop `b`, then `a`, apply `f`, push the result.
    fn binop<F: Fn(Value, Value) -> Value>(&mut self, f: F) {
        let b = self.top().stack.pop();
        let a = self.top().stack.pop();
        self.top().stack.push(f(a, b));
    }

    fn cmp<F: Fn(Value, Value) -> bool>(&mut self, f: F) {
        let b = self.top().stack.pop();
        let a = self.top().stack.pop();
        self.top().stack.push(Value::boolean(f(a, b)));
    }

    fn unary<F: Fn(Value) -> Value>(&mut self, f: F) {
        let a = self.top().stack.pop();
        self.top().stack.push(f(a));
    }

    /// Build a [`JsFunction`] value for function-table index `id`, capturing
    /// upvalue cells from the currently executing frame per the function's
    /// compiled [`UpvalueSpec`]s.
    fn function_value(
        &mut self,
        module: &BytecodeModule,
        module_index: usize,
        id: u32,
    ) -> JsFunction {
        let func = func_ref(module, id as usize);
        let is_arrow = func.is_arrow;
        // Capture upvalue cells from the current (enclosing) frame.
        let mut upvalues = Vec::with_capacity(func.upvalues.len());
        for spec in &func.upvalues {
            let frame = self.frames.last().unwrap();
            let cell = if spec.is_local {
                frame
                    .locals
                    .get(spec.index as usize)
                    .cloned()
                    .or_else(|| {
                        self.deferred_modules
                            .as_ref()
                            .and_then(|graph| graph.locals.get(frame.module_index))
                            .and_then(|locals| locals.get(spec.index as usize))
                            .cloned()
                    })
                    .expect("captured local must exist in the frame or module environment")
            } else {
                frame.upvalues[spec.index as usize].clone()
            };
            upvalues.push(cell);
        }
        // Arrows capture `this` lexically from the enclosing frame.
        let this_cell = if is_arrow {
            let frame = self.frames.last().unwrap();
            Some(
                frame
                    .captured_this
                    .clone()
                    .unwrap_or_else(|| frame.this_binding.clone()),
            )
        } else {
            None
        };
        let mut f = JsFunction::new(func.name.clone(), id, func.param_count);
        let object_prototype = self
            .ctx
            .realm
            .borrow()
            .globals
            .get("Object")
            .map(|object| get_property(object, &Value::string("prototype")));
        if let Some(object_prototype) = object_prototype {
            f.object.borrow_mut().proto = Some(object_prototype.clone());
            let function_prototype = f.object.borrow().properties.get("prototype").and_then(
                |descriptor| match descriptor {
                    PropertyDescriptor::Data { value, .. } => obj_as_object(value).cloned(),
                    PropertyDescriptor::Accessor { .. } => None,
                },
            );
            if let Some(function_prototype) = function_prototype {
                function_prototype.borrow_mut().proto = Some(object_prototype);
            }
        }
        let function_value = Value::function(f.clone());
        if let Some(function_prototype) =
            f.object
                .borrow()
                .properties
                .get("prototype")
                .and_then(|descriptor| match descriptor {
                    PropertyDescriptor::Data { value, .. } => obj_as_object(value).cloned(),
                    PropertyDescriptor::Accessor { .. } => None,
                })
        {
            function_prototype.borrow_mut().properties.insert(
                "constructor".into(),
                PropertyDescriptor::Data {
                    value: function_value,
                    attr: js_runtime::object::Attribute {
                        writable: true,
                        enumerable: false,
                        configurable: true,
                    },
                },
            );
        }
        f.module_index = module_index as u32;
        f.upvalues = upvalues;
        f.with_environments = self
            .frames
            .last()
            .map(|frame| frame.with_environments.clone())
            .unwrap_or_default();
        f.this_cell = this_cell;
        f.is_generator = func.is_generator;
        if is_arrow {
            f.superclass = self
                .frames
                .last()
                .and_then(|frame| frame.superclass.clone())
                .map(Box::new);
            f.home_object = self
                .frames
                .last()
                .and_then(|frame| frame.home_object.clone())
                .map(Box::new);
            f.reject_eval_arguments = self
                .frames
                .last()
                .is_some_and(|frame| frame.reject_eval_arguments);
        }
        f.private_brands = self
            .frames
            .last()
            .map(|frame| frame.private_brands.clone())
            .unwrap_or_default();
        if !f.private_brands.contains_key(&id) {
            let brand = self.next_private_brand;
            self.next_private_brand += 1;
            f.private_brands.insert(id, brand);
        }
        f
    }

    /// The `this` value for the current frame (ordinary frame `this`, or the
    /// arrow's lexically captured `this`).
    fn current_this(&self) -> Result<Value, InterpError> {
        let frame = self.frames.last().unwrap();
        if let Some(c) = &frame.captured_this {
            return c.get().map_err(binding_error_value);
        }
        frame.this_binding.get().map_err(binding_error_value)
    }

    /// ECMAScript ToPropertyKey with the String hint. Class computed names use
    /// this at definition time, so user conversion code and abrupt completion
    /// cannot be deferred until an instance is created.
    fn to_property_key_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        value: Value,
    ) -> Result<Value, InterpError> {
        match value.data() {
            ValueData::Symbol(_) => return Ok(value),
            ValueData::Object(_) | ValueData::Function(_) | ValueData::Generator(_) => {}
            _ => return Ok(Value::string(to_string(&value))),
        }

        let object = obj_as_object(&value).cloned();
        let exotic_key = Value::symbol(js_runtime::value::JsSymbol::to_primitive());
        let exotic = self.get_property_value(modules, &value, &exotic_key, &value)?;
        if !exotic.is_nullish() {
            if !exotic.is_function() {
                return Err(InterpError::Throw(type_error(
                    "@@toPrimitive is not callable",
                )));
            }
            let primitive = self.call_value(
                modules,
                exotic,
                vec![Value::string("string")],
                value.clone(),
            )?;
            return match primitive.data() {
                ValueData::Object(_) | ValueData::Function(_) | ValueData::Generator(_) => Err(
                    InterpError::Throw(type_error("@@toPrimitive returned an object")),
                ),
                ValueData::Symbol(_) => Ok(primitive),
                _ => Ok(Value::string(to_string(&primitive))),
            };
        }
        let explicit_null_prototype = object
            .as_ref()
            .is_some_and(|object| object.borrow().explicit_null_prototype);
        let mut had_explicit_conversion = false;
        for name in ["toString", "valueOf"] {
            let has_own = object
                .as_ref()
                .is_some_and(|object| object.borrow().properties.contains_key(name));
            had_explicit_conversion |= has_own;
            let method = self.get_property_value(modules, &value, &Value::string(name), &value)?;
            if method.as_function().is_none() {
                continue;
            }
            let primitive = self.call_value(modules, method, Vec::new(), value.clone())?;
            match primitive.data() {
                ValueData::Symbol(_) => return Ok(primitive),
                ValueData::Object(_) | ValueData::Function(_) | ValueData::Generator(_) => {}
                _ => return Ok(Value::string(to_string(&primitive))),
            }
        }
        if explicit_null_prototype || had_explicit_conversion {
            Err(InterpError::Throw(type_error(
                "cannot convert object to property key",
            )))
        } else {
            // Ordinary Object.prototype.toString is represented as a VM
            // fallback until intrinsic prototype objects are fully wired.
            Ok(Value::string(to_string(&value)))
        }
    }

    /// Parse, compile and execute ECMAScript eval code in the current realm.
    /// Direct eval additionally inherits `this`, class private brands and the
    /// syntactic permissions of the active class execution context. Lexical
    /// binding cells are intentionally kept behind this boundary so the next
    /// environment-record step can add them without coupling Parser to the VM.
    pub(crate) fn eval_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        input: Value,
        direct: bool,
    ) -> Result<Value, InterpError> {
        let source_text = match input.data() {
            ValueData::String(value) => value.as_str().to_string(),
            _ => return Ok(input),
        };

        let mut private_names = HashMap::new();
        let mut inherited_brands = HashMap::new();
        let mut this_value = Value::undefined();
        let mut superclass = None;
        let mut function_name = "<main>".to_string();
        let mut outer_bindings: HashMap<String, js_runtime::value::Cell> = HashMap::new();
        let mut global_var_cells = Vec::new();
        if direct {
            let frame = self.frames.last().ok_or_else(|| {
                InterpError::Internal("direct eval has no active execution context".into())
            })?;
            inherited_brands = frame.private_brands.clone();
            this_value = self.current_this()?;
            superclass = frame.superclass.clone();
            if let Some(module_ptr) = self.module_ptr(modules, frame.module_index) {
                let module = unsafe { &*module_ptr };
                let function = func_ref(module, frame.func_index);
                function_name = function.name.clone();
                for (name, slot) in function.locals.entries() {
                    if let Some(cell) = frame.locals.get(slot as usize) {
                        outer_bindings.insert(name.to_string(), cell.clone());
                    }
                }
                for (name, cell) in function.upvalue_names.iter().zip(&frame.upvalues) {
                    outer_bindings
                        .entry(name.clone())
                        .or_insert_with(|| cell.clone());
                }
                for constant in module.constants.items() {
                    let ValueData::String(encoded) = constant.data() else {
                        continue;
                    };
                    let Some((class_id, description)) = encoded.as_str().split_once('\0') else {
                        continue;
                    };
                    let Ok(class_id) = class_id.parse::<u32>() else {
                        continue;
                    };
                    if inherited_brands.contains_key(&class_id) {
                        private_names.insert(description.to_string(), class_id);
                    }
                }
            }
        } else {
            // Script `var` bindings belong to the Global Environment Record.
            // The baseline VM stores them in the entry frame for slot speed;
            // expose those same cells to indirect eval's global environment.
            for frame in &self.frames {
                let Some(module_ptr) = self.module_ptr(modules, frame.module_index) else {
                    continue;
                };
                let module = unsafe { &*module_ptr };
                if frame.func_index != 0 || module.is_module {
                    continue;
                }
                for (name, slot) in module.main.locals.entries() {
                    if let Some(cell) = frame.locals.get(slot as usize) {
                        if let Ok(value) = cell.get() {
                            self.ctx
                                .realm
                                .borrow_mut()
                                .globals
                                .insert(name.to_string(), value);
                            global_var_cells.push((name.to_string(), cell.clone()));
                        }
                    }
                }
                break;
            }
        }

        let inside_initializer = function_name == "<class-instance-initializer>"
            || function_name == "<class-static-initializer>";
        let parse_context = js_parser::early_errors::EvalContext {
            strict: direct && !private_names.is_empty(),
            private_names: private_names.keys().cloned().collect(),
            allow_super_property: direct && superclass.is_some(),
            allow_super_call: false,
            allow_new_target: direct,
            reject_arguments: direct
                && (inside_initializer
                    || self
                        .frames
                        .last()
                        .is_some_and(|frame| frame.reject_eval_arguments)),
        };
        let source = std::sync::Arc::new(js_syntax::SourceFile::new(
            "<eval>",
            std::sync::Arc::<str>::from(source_text.as_str()),
        ));
        let sess = js_parser::ParseSess::from_shared(source.clone());
        let program = js_parser::Parser::new(&sess)
            .parse_syntax(js_syntax::ProgramKind::Script)
            .and_then(|program| {
                let mut errors = js_parser::early_errors::check_eval(&program, &parse_context);
                for diagnostic in &mut errors {
                    diagnostic.classify(js_diagnostics::DiagnosticPhase::EarlyError, "JS-EARLY");
                }
                if errors.is_empty() {
                    Ok(program)
                } else {
                    Err(errors)
                }
            })
            .map_err(|errors| {
                let message = errors
                    .iter()
                    .map(|diagnostic| diagnostic.message.clone())
                    .collect::<Vec<_>>()
                    .join("; ");
                InterpError::Throw(type_error_named("SyntaxError", &message))
            })?;
        let bytecode =
            js_bytecode::compile_eval_program_with_source(&program, source, private_names, {
                let mut names: Vec<_> = outer_bindings.keys().cloned().collect();
                names.sort();
                names
            })
            .map_err(|errors| {
                let message = errors
                    .iter()
                    .map(|diagnostic| diagnostic.message.clone())
                    .collect::<Vec<_>>()
                    .join("; ");
                InterpError::Throw(type_error_named("SyntaxError", &message))
            })?;

        let mut ordered_bindings: Vec<_> = outer_bindings.into_iter().collect();
        ordered_bindings.sort_by(|left, right| left.0.cmp(&right.0));
        let outer_cells = ordered_bindings.into_iter().map(|(_, cell)| cell).collect();
        let eval_module_index = modules.len() + self.eval_modules.len();
        self.eval_modules
            .push((eval_module_index, Box::new(bytecode)));
        let result = self.run_eval_bytecode(
            modules,
            eval_module_index,
            this_value,
            inherited_brands,
            superclass,
            outer_cells,
        );
        for (name, cell) in global_var_cells {
            if let Some(value) = self.ctx.realm.borrow().globals.get(&name).cloned() {
                let _ = cell.set(value);
            }
        }
        result
    }

    fn run_eval_bytecode(
        &mut self,
        enclosing_modules: &BytecodeGraph<'_>,
        eval_module_index: usize,
        this_value: Value,
        private_brands: HashMap<u32, u64>,
        superclass: Option<Value>,
        outer_cells: Vec<js_runtime::value::Cell>,
    ) -> Result<Value, InterpError> {
        let module_ptr = self
            .module_ptr(enclosing_modules, eval_module_index)
            .ok_or_else(|| InterpError::Internal("eval bytecode was not retained".into()))?;
        let module = unsafe { &*module_ptr };
        js_bytecode::verify_module(module).map_err(|errors| {
            InterpError::Internal(format!("invalid eval bytecode: {}", errors[0]))
        })?;
        let caller_depth = self.frames.len();
        let mut frame = CallFrame::for_module(
            eval_module_index,
            0,
            module.main.locals.slot_count(),
            module.main.span,
        );
        frame.this = this_value.clone();
        frame.this_binding = js_runtime::value::Cell::mutable(this_value);
        frame.private_brands = private_brands;
        frame.superclass = superclass;
        frame.upvalues = module
            .main
            .upvalues
            .iter()
            .map(|spec| {
                if !spec.is_local {
                    return Err(InterpError::Internal(
                        "eval outer binding unexpectedly references an upvalue".into(),
                    ));
                }
                outer_cells
                    .get(spec.index as usize)
                    .cloned()
                    .ok_or_else(|| {
                        InterpError::Internal(format!(
                            "eval outer binding slot {} is missing",
                            spec.index
                        ))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.frames.push(frame);
        while self.frames.len() > caller_depth {
            match self.step(enclosing_modules) {
                Ok(Step::More) => {}
                Ok(Step::Done(value)) => return Ok(value),
                Ok(Step::Suspend(_)) => {
                    return Err(InterpError::Internal(
                        "await is not supported in classic eval code".into(),
                    ))
                }
                Err(error) => match self.handle_exception(enclosing_modules, error, caller_depth) {
                    Ok(()) => {}
                    Err(error) => return Err(error),
                },
            }
        }
        Ok(self.top().stack.pop())
    }

    fn private_name(
        &self,
        module: &BytecodeModule,
        operand: u16,
    ) -> Result<js_runtime::object::PrivateName, InterpError> {
        let encoded = match module.constants.get(operand).data() {
            ValueData::String(value) => value.as_str(),
            _ => {
                return Err(InterpError::Internal(
                    "private-name operand is not a string constant".into(),
                ))
            }
        };
        let (class_id, description) = encoded.split_once('\0').ok_or_else(|| {
            InterpError::Internal("private-name constant has no class identity".into())
        })?;
        let class_id = class_id.parse::<u32>().map_err(|_| {
            InterpError::Internal("private-name constant has an invalid class identity".into())
        })?;
        let brand = self
            .frames
            .last()
            .and_then(|frame| frame.private_brands.get(&class_id))
            .copied()
            .ok_or_else(|| {
                InterpError::Internal(format!(
                    "private environment for class function {class_id} was not captured"
                ))
            })?;
        Ok(js_runtime::object::PrivateName {
            brand,
            description: description.to_string(),
        })
    }

    /// Push a new frame for `callee` with the given `this` binding and args.
    /// `is_construct` marks a `new` invocation, which keeps the fresh object as
    /// the result unless the constructor explicitly returns an object.
    fn invoke(
        &mut self,
        modules: &BytecodeGraph<'_>,
        callee: Value,
        args: Vec<Value>,
        this: Value,
        is_construct: bool,
    ) {
        let f = match callee.as_function() {
            Some(f) => f.clone(),
            None => {
                self.pending_err = Some(InterpError::Throw(type_error(if is_construct {
                    "value is not a constructor"
                } else {
                    "value is not callable"
                })));
                return;
            }
        };
        let mut args = args;
        if !f.bound_args.is_empty() {
            let mut combined = f.bound_args.clone();
            combined.extend(args);
            args = combined;
        }
        let this = if is_construct {
            this
        } else {
            f.bound_this.as_deref().cloned().unwrap_or(this)
        };
        // Bound recursion depth: JS calls drive Rust recursion through
        // `step → invoke → call_value → step`, so this prevents a native
        // stack-overflow abort on deep or runaway recursion.
        if self.frames.len() >= MAX_FRAMES {
            self.pending_err = Some(InterpError::Internal("maximum call depth exceeded".into()));
            return;
        }
        // Native (builtin) function: run it and handle the result.
        if let Some(nid) = f.native {
            // A native call made from `finally` must not erase the stack of an
            // exception already propagating through that finally block. Give
            // the native invocation a clean trace, then restore the prior one
            // only when the native call completes normally.
            let prior_trace = std::mem::take(&mut self.error_trace);
            // SAFETY: a native call never mutates `self.natives` (the table is
            // fixed after construction), so a `*const` borrow is stable for the
            // call's duration even though `self` is borrowed mutably inside it.
            let nf_ptr: *const dyn NativeFn = self.natives[nid as usize].as_ref();
            let result = unsafe { (&*nf_ptr).call(self, modules, this, &f, args, is_construct) };
            match result {
                Ok(NativeResult::Value(v)) => {
                    self.error_trace = prior_trace;
                    self.top().stack.push(v)
                }
                Ok(NativeResult::ResumeGenerator(gen, kind, arg)) => {
                    self.error_trace = prior_trace;
                    self.checkout_generator(gen, kind, arg);
                }
                Ok(NativeResult::ResumeAsyncGenerator(gen, kind, arg, promise)) => {
                    self.error_trace = prior_trace;
                    self.checkout_generator(gen, kind, arg);
                    self.top().async_generator_promise = Some(promise);
                }
                Err(e) => self.pending_err = Some(e),
            }
            return;
        }
        let module_index = f.module_index as usize;
        let Some(module_ptr) = self.module_ptr(modules, module_index) else {
            self.pending_err = Some(InterpError::Internal(format!(
                "function refers to missing bytecode module {module_index}"
            )));
            return;
        };
        let module = unsafe { &*module_ptr };
        let id = f.id as usize;
        let func = func_ref(module, id);
        // Calling a `function*` creates a generator object (it does not run).
        if func.is_generator && !is_construct {
            let state = self.make_generator_state(module, module_index, id, &f, args, this);
            self.top()
                .stack
                .push(Value::generator(Rc::new(RefCell::new(state))));
            return;
        }
        let (slot_count, param_count, span) = func_meta(module, id);
        let mut nf = CallFrame::for_module(module_index, id, slot_count, span);
        nf.arguments = args.clone();
        for i in 0..(param_count as usize).min(args.len()) {
            nf.locals[i] = new_cell(args[i].clone());
        }
        nf.upvalues = f.upvalues.clone();
        nf.with_environments = f.with_environments.clone();
        if func.is_async {
            nf.async_promise = Some(crate::builtins::promise_pending());
        }
        if f.this_cell.is_some() {
            nf.captured_this = f.this_cell.clone();
        } else {
            nf.this = this.clone();
            nf.this_binding = if is_construct && f.superclass.is_some() {
                js_runtime::value::Cell::uninitialized(true)
            } else {
                js_runtime::value::Cell::mutable(this)
            };
        }
        nf.is_construct = is_construct;
        nf.constructor = is_construct.then(|| callee.clone());
        nf.superclass = f.superclass.as_deref().cloned();
        nf.home_object = f.home_object.as_deref().cloned();
        nf.reject_eval_arguments = f.reject_eval_arguments
            || matches!(
                func.name.as_str(),
                "<class-instance-initializer>" | "<class-static-initializer>"
            );
        nf.private_brands = f.private_brands;
        nf.class_field_keys = f.class_field_keys;
        self.frames.push(nf);
    }

    /// Build the suspended state for a freshly-called generator function.
    fn make_generator_state(
        &self,
        module: &BytecodeModule,
        module_index: usize,
        id: usize,
        f: &JsFunction,
        args: Vec<Value>,
        this: Value,
    ) -> GeneratorState {
        let func = func_ref(module, id);
        let slot_count = func.locals.slot_count();
        let mut locals: Vec<_> = (0..slot_count)
            .map(|_| new_cell(Value::undefined()))
            .collect();
        for i in 0..(func.param_count as usize).min(args.len()) {
            locals[i] = new_cell(args[i].clone());
        }
        GeneratorState {
            module_index: module_index as u32,
            func_index: id as u32,
            pc: 0,
            locals,
            stack: Vec::new(),
            upvalues: f.upvalues.clone(),
            with_environments: f.with_environments.clone(),
            private_brands: f.private_brands.clone(),
            private_environment_stack: Vec::new(),
            this: if f.this_cell.is_some() {
                Value::undefined()
            } else {
                this
            },
            captured_this: f.this_cell.clone(),
            home_object: f.home_object.as_deref().cloned(),
            reject_eval_arguments: f.reject_eval_arguments,
            is_async: func.is_async,
            delegate: None,
            try_stack: Vec::new(),
            pending_throw: None,
            async_executing: false,
            async_queue: VecDeque::new(),
            done: false,
            started: false,
        }
    }

    /// Resume a paused generator: check its frame state out into a live
    /// `CallFrame`, push the `.next(arg)` argument (for non-first resumes), and
    /// push the frame so the dispatch loop continues it.
    fn checkout_generator(
        &mut self,
        gen: Rc<RefCell<GeneratorState>>,
        kind: GeneratorResumeKind,
        arg: Value,
    ) {
        let (
            done,
            started,
            module_index,
            func_index,
            pc,
            locals,
            stack,
            upvalues,
            with_environments,
            private_brands,
            private_environment_stack,
            this,
            captured_this,
            home_object,
            reject_eval_arguments,
            try_stack,
            pending_throw,
        ) = {
            let mut s = gen.borrow_mut();
            if s.done {
                // Already finished: return {undefined, true}.
                drop(s);
                self.top().stack.push(iter_result(Value::undefined(), true));
                return;
            }
            let started = s.started;
            s.started = true;
            (
                false,
                started,
                s.module_index,
                s.func_index,
                s.pc,
                std::mem::take(&mut s.locals),
                std::mem::take(&mut s.stack),
                std::mem::take(&mut s.upvalues),
                std::mem::take(&mut s.with_environments),
                std::mem::take(&mut s.private_brands),
                std::mem::take(&mut s.private_environment_stack),
                std::mem::replace(&mut s.this, Value::undefined()),
                s.captured_this.take(),
                s.home_object.take(),
                s.reject_eval_arguments,
                std::mem::take(&mut s.try_stack),
                s.pending_throw.take(),
            )
        };
        let _ = done;
        let mut frame = CallFrame::for_module(
            module_index as usize,
            func_index as usize,
            0,
            js_syntax::Span::DUMMY,
        );
        frame.pc = pc;
        frame.locals = locals;
        frame.upvalues = upvalues;
        frame.with_environments = with_environments;
        frame.private_brands = private_brands;
        frame.private_environment_stack = private_environment_stack;
        frame.this = this.clone();
        frame.this_binding = js_runtime::value::Cell::mutable(this);
        frame.captured_this = captured_this;
        frame.home_object = home_object;
        frame.reject_eval_arguments = reject_eval_arguments;
        frame.generator = Some(gen);
        frame.try_stack = try_stack
            .into_iter()
            .map(|state| crate::frame::ActiveTry {
                catch_pc: state.catch_pc,
                finally_pc: state.finally_pc,
            })
            .collect();
        frame.pending_throw = pending_throw;
        for v in stack {
            frame.stack.push(v);
        }
        if started {
            frame.generator_resume = Some((kind, arg));
        }
        self.frames.push(frame);
    }
}

/// Build the default native-function table (generator `.next`/`.return`/`.
/// throw`, then the Array/String builtin methods).
fn default_natives() -> Vec<Box<dyn NativeFn>> {
    let mut v: Vec<Box<dyn NativeFn>> =
        vec![Box::new(GenNext), Box::new(GenReturn), Box::new(GenThrow)];
    v.extend(crate::builtins::all_builtins());
    v
}

/// Construct an iterator-result object `{ value, done }`.
pub(crate) fn iter_result(value: Value, done: bool) -> Value {
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.properties.insert(
            "value".to_string(),
            js_runtime::object::PropertyDescriptor::data(value),
        );
        b.properties.insert(
            "done".to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::boolean(done)),
        );
    }
    Value::object(o)
}

/// Pop `n` args and the callee from the current frame's operand stack.
fn pop_args(interp: &mut Interpreter, n: u16) -> (Value, Vec<Value>) {
    let frame = interp.frames.last_mut().unwrap();
    let n = n as usize;
    let mut args: Vec<Value> = (0..n).map(|_| frame.stack.pop()).collect();
    args.reverse();
    let callee = frame.stack.pop();
    (callee, args)
}

fn argument_list_values(arguments: &Value) -> Result<Vec<Value>, InterpError> {
    let object = obj_as_object(arguments)
        .ok_or_else(|| InterpError::Internal("dynamic argument list is not an Array".into()))?;
    let length = match get_property(arguments, &Value::string("length")).data() {
        ValueData::Integer(value) => *value as usize,
        ValueData::Number(value) => *value as usize,
        _ => 0,
    };
    let _ = object;
    Ok((0..length)
        .map(|index| get_property(arguments, &Value::string(index.to_string())))
        .collect())
}

fn inherit_method_context(receiver: &Value, callee: &mut Value) {
    let (Some(receiver), Some(target)) = (receiver.as_function(), callee.as_function_mut()) else {
        return;
    };
    for (&class_id, &brand) in &receiver.private_brands {
        target.private_brands.insert(class_id, brand);
    }
    target.class_field_keys = receiver.class_field_keys.clone();
    target.superclass = receiver.superclass.clone();
    if target.name == "<class-static-initializer>" {
        target.home_object = Some(Box::new(Value::function(receiver.clone())));
    }
}

fn func_ref<'a>(module: &'a BytecodeModule, index: usize) -> &'a BytecodeFunction {
    if index == 0 {
        &module.main
    } else {
        &module.functions[index - 1]
    }
}

fn runtime_frame(module: &BytecodeModule, frame: &CallFrame) -> RuntimeFrame {
    RuntimeFrame {
        function: func_ref(module, frame.func_index).name.clone(),
        span: frame.span,
        source: module.source.clone(),
    }
}

/// `(slot_count, param_count, span)` for a function by table id.
fn func_meta(module: &BytecodeModule, id: usize) -> (u16, u16, js_syntax::Span) {
    let f = func_ref(module, id);
    (f.locals.slot_count(), f.param_count, f.span)
}

// ---- value semantics (milestone subset) ----------------------------------

fn as_f64(v: &ValueData) -> Option<f64> {
    match v {
        ValueData::Number(n) => Some(*n),
        ValueData::Integer(i) => Some(*i as f64),
        ValueData::Boolean(b) => Some(if *b { 1.0 } else { 0.0 }),
        ValueData::Null => Some(0.0),
        ValueData::Undefined => Some(f64::NAN),
        _ => None,
    }
}

fn num_value(v: Value) -> Value {
    match v.data().clone() {
        ValueData::Integer(i) => Value::integer(i),
        ValueData::Number(n) => Value::number(n),
        other => match as_f64(&other) {
            Some(n) if n.fract() == 0.0 && n.is_finite() && n.abs() < i32::MAX as f64 => {
                Value::integer(n as i32)
            }
            Some(n) => Value::number(n),
            None => Value::number(f64::NAN),
        },
    }
}

fn add(a: Value, b: Value) -> Value {
    use ValueData::*;
    match (a.data().clone(), b.data().clone()) {
        // String concatenation: if either side is a string, coerce both to string.
        (String(_), _) | (_, String(_)) => Value::string(to_string(&a) + &to_string(&b)),
        (Integer(x), Integer(y)) => match x.checked_add(y) {
            Some(z) => Value::integer(z),
            None => Value::number(x as f64 + y as f64),
        },
        _ => Value::number(num_f64(&a) + num_f64(&b)),
    }
}

fn sub(a: Value, b: Value) -> Value {
    if let (Some(x), Some(y)) = (as_int(&a), as_int(&b)) {
        if let Some(z) = x.checked_sub(y) {
            return Value::integer(z);
        }
    }
    Value::number(num_f64(&a) - num_f64(&b))
}

fn mul(a: Value, b: Value) -> Value {
    if let (Some(x), Some(y)) = (as_int(&a), as_int(&b)) {
        if let Some(z) = x.checked_mul(y) {
            return Value::integer(z);
        }
    }
    Value::number(num_f64(&a) * num_f64(&b))
}

fn div(a: Value, b: Value) -> Value {
    Value::number(num_f64(&a) / num_f64(&b))
}

fn rem(a: Value, b: Value) -> Value {
    Value::number(num_f64(&a) % num_f64(&b))
}

fn pow(a: Value, b: Value) -> Value {
    Value::number(num_f64(&a).powf(num_f64(&b)))
}

fn as_int(v: &Value) -> Option<i32> {
    match v.data() {
        ValueData::Integer(i) => Some(*i),
        ValueData::Number(n) if n.fract() == 0.0 && n.is_finite() => Some(*n as i32),
        _ => None,
    }
}

fn num_f64(v: &Value) -> f64 {
    as_f64(v.data()).unwrap_or(f64::NAN)
}

fn bitand(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a) & to_int32(&b))
}
fn bitor(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a) | to_int32(&b))
}
fn bitxor(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a) ^ to_int32(&b))
}
fn shl(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a).wrapping_shl((to_uint32(&b) & 31) as u32))
}
fn shr(a: Value, b: Value) -> Value {
    Value::integer(to_int32(&a).wrapping_shr((to_uint32(&b) & 31) as u32))
}
fn ushr(a: Value, b: Value) -> Value {
    let result = to_uint32(&a).wrapping_shr(to_uint32(&b) & 31);
    if result <= i32::MAX as u32 {
        Value::integer(result as i32)
    } else {
        Value::number(result as f64)
    }
}

fn to_int32(v: &Value) -> i32 {
    let value = to_uint32(v);
    if value >= 0x8000_0000 {
        (value as i64 - 0x1_0000_0000) as i32
    } else {
        value as i32
    }
}
fn to_uint32(v: &Value) -> u32 {
    let number = num_f64(v);
    if !number.is_finite() || number == 0.0 {
        return 0;
    }
    // ECMA-262 ToUint32 truncates toward zero, then reduces modulo 2^32.
    number.trunc().rem_euclid(4_294_967_296.0) as u32
}

fn neg(a: Value) -> Value {
    match a.data() {
        ValueData::Integer(i) => Value::integer(i.wrapping_neg()),
        _ => Value::number(-num_f64(&a)),
    }
}
fn pos(a: Value) -> Value {
    num_value(a)
}
fn bitnot(a: Value) -> Value {
    Value::integer(!to_int32(&a))
}
fn typeof_(a: Value) -> Value {
    let s = match a.data() {
        ValueData::Undefined => "undefined",
        ValueData::Null => "object",
        ValueData::Boolean(_) => "boolean",
        ValueData::Number(_) | ValueData::Integer(_) => "number",
        ValueData::String(_) => "string",
        ValueData::Function(_) => "function",
        ValueData::Symbol(_) => "symbol",
        ValueData::BigInt(_) => "bigint",
        ValueData::Object(_) => "object",
        ValueData::Generator(_) => "object",
    };
    Value::string(s)
}

pub(crate) fn eq_strict(a: Value, b: Value) -> bool {
    use ValueData::*;
    match (a.data(), b.data()) {
        (Integer(x), Integer(y)) => x == y,
        (Integer(x), Number(y)) => (*x as f64) == *y,
        (Number(x), Integer(y)) => *x == (*y as f64),
        (Number(x), Number(y)) => x == y,
        (String(x), String(y)) => x == y,
        (Boolean(x), Boolean(y)) => x == y,
        (Null, Null) | (Undefined, Undefined) => true,
        (Object(x), Object(y)) => Rc::ptr_eq(x, y),
        (Function(x), Function(y)) => Rc::ptr_eq(&x.object, &y.object),
        (Symbol(x), Symbol(y)) => x.id == y.id,
        (BigInt(x), BigInt(y)) => x == y,
        (Symbol(_), _) | (_, Symbol(_)) => false,
        _ => false,
    }
}

fn eq_loose(a: Value, b: Value) -> bool {
    use ValueData::*;
    match (a.data(), b.data()) {
        (Undefined, Null) | (Null, Undefined) => true,
        // Same type → fall back to strict equality.
        _ if std::mem::discriminant(a.data()) == std::mem::discriminant(b.data()) => {
            eq_strict(a, b)
        }
        // Otherwise: loose numeric coercion for the milestone subset.
        _ => {
            let na = as_f64(a.data());
            let nb = as_f64(b.data());
            match (na, nb) {
                (Some(x), Some(y)) => x == y,
                _ => false,
            }
        }
    }
}

fn cmp_lt(a: Value, b: Value) -> bool {
    num_f64(&a) < num_f64(&b)
}
fn cmp_le(a: Value, b: Value) -> bool {
    num_f64(&a) <= num_f64(&b)
}
fn cmp_gt(a: Value, b: Value) -> bool {
    num_f64(&a) > num_f64(&b)
}
fn cmp_ge(a: Value, b: Value) -> bool {
    num_f64(&a) >= num_f64(&b)
}

fn is_truthy(v: &Value) -> bool {
    v.to_boolean()
}

fn is_falsy(v: &Value) -> bool {
    !v.to_boolean()
}

pub(crate) fn to_string(v: &Value) -> String {
    match v.data() {
        ValueData::Undefined => "undefined".to_string(),
        ValueData::Null => "null".to_string(),
        ValueData::Boolean(b) => b.to_string(),
        ValueData::Integer(i) => i.to_string(),
        ValueData::Number(n) => format_number(*n),
        ValueData::String(s) => s.as_str().to_string(),
        ValueData::Symbol(symbol) => format!(
            "Symbol({})",
            symbol.description.as_deref().unwrap_or_default()
        ),
        ValueData::Object(o) => {
            let b = o.borrow();
            if b.is_exotic_array {
                // Array.prototype.toString === join(",") of elements.
                let len = b
                    .properties
                    .get("length")
                    .and_then(|d| match d {
                        js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                            match value.data() {
                                ValueData::Integer(i) => Some(*i as usize),
                                ValueData::Number(n) => Some(*n as usize),
                                _ => None,
                            }
                        }
                        _ => None,
                    })
                    .unwrap_or(0);
                let parts: Vec<String> = (0..len)
                    .map(|i| {
                        let e = b
                            .properties
                            .get(&i.to_string())
                            .and_then(|d| match d {
                                js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                                    Some(value.clone())
                                }
                                _ => None,
                            })
                            .unwrap_or_else(Value::undefined);
                        if e.is_null() || e.is_undefined() {
                            String::new()
                        } else {
                            to_string(&e)
                        }
                    })
                    .collect();
                drop(b);
                parts.join(",")
            } else {
                // Error-like objects: if they have `name` + `message`,
                // display as "name: message" for readable output.
                let (name_v, msg_v) = {
                    let nv = b.properties.get("name").and_then(|d| match d {
                        js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                            Some(value.clone())
                        }
                        _ => None,
                    });
                    let mv = b.properties.get("message").and_then(|d| match d {
                        js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                            Some(value.clone())
                        }
                        _ => None,
                    });
                    (nv, mv)
                };
                drop(b);
                match (name_v, msg_v) {
                    (Some(nv), Some(mv)) => {
                        let n = to_string(&nv);
                        let m = to_string(&mv);
                        if n.ends_with("Error") {
                            if m.is_empty() {
                                n
                            } else {
                                format!("{}: {}", n, m)
                            }
                        } else {
                            "[object Object]".to_string()
                        }
                    }
                    _ => "[object Object]".to_string(),
                }
            }
        }
        _ => "[object Object]".to_string(),
    }
}

fn format_number(n: f64) -> String {
    if n.is_nan() {
        "NaN".to_string()
    } else if n == f64::INFINITY {
        "Infinity".to_string()
    } else if n == f64::NEG_INFINITY {
        "-Infinity".to_string()
    } else if n != 0.0 && (n.abs() < 1e-6 || n.abs() >= 1e21) {
        let scientific = format!("{n:e}");
        if let Some((mantissa, exponent)) = scientific.split_once('e') {
            let exponent = exponent.parse::<i32>().unwrap_or(0);
            format!("{mantissa}e{exponent:+}")
        } else {
            scientific
        }
    } else if n.fract() == 0.0 && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{}", n)
    }
}

/// Convert a property-key value to its canonical string name.
pub(crate) fn prop_name(key: &Value) -> String {
    match key.data() {
        ValueData::String(s) => s.as_str().to_string(),
        ValueData::Integer(i) => i.to_string(),
        ValueData::Number(n) => format_number(*n),
        ValueData::Boolean(b) => b.to_string(),
        ValueData::Null => "null".to_string(),
        ValueData::Undefined => "undefined".to_string(),
        _ => to_string(key),
    }
}

/// `obj[key]` / `obj.key`. Walks the prototype chain; returns `undefined` for
/// missing properties (no throw in the milestone subset). Strings expose
/// `.length` and integer indexing.
pub(crate) fn get_property(obj: &Value, key: &Value) -> Value {
    get_property_checked(obj, key).unwrap_or_else(|_| Value::undefined())
}

fn get_property_checked(
    obj: &Value,
    key: &Value,
) -> Result<Value, js_runtime::value::BindingError> {
    if let ValueData::Symbol(symbol) = key.data() {
        let Some(handle) = obj_as_object(obj) else {
            return Ok(Value::undefined());
        };
        let mut current = Some(handle.clone());
        while let Some(object) = current {
            let data = object.borrow();
            if let Some(PropertyDescriptor::Data { value, .. }) =
                data.symbol_properties.get(&symbol.id)
            {
                return Ok(value.clone());
            }
            current = data.proto.as_ref().and_then(obj_as_object).cloned();
        }
        return Ok(Value::undefined());
    }
    let name = prop_name(key);
    if let Some(value) = crate::builtins::native_static_value(obj, &name) {
        return Ok(value);
    }
    // Static methods on global constructor functions (Number.isInteger etc.).
    if let Some(nid) = crate::builtins::native_static_id(obj, &name) {
        let mut f = JsFunction::new(name.clone(), 0, 1);
        f.native = Some(nid);
        return Ok(Value::function(f));
    }
    // Known builtin instance methods on arrays/strings resolve to native fns.
    if let Some(bid) = crate::builtins::builtin_method_id(obj, &name) {
        let mut f = JsFunction::new(name.clone(), 0, 1);
        f.native = Some(bid);
        return Ok(Value::function(f));
    }
    // Generator objects expose `.next` / `.return` / `.throw` as bound native
    // methods.
    if let ValueData::Generator(g) = obj.data() {
        let native = match name.as_str() {
            "next" => Some(native_id::GEN_NEXT),
            "return" => Some(native_id::GEN_RETURN),
            "throw" => Some(native_id::GEN_THROW),
            _ => None,
        };
        if let Some(nid) = native {
            let mut f = JsFunction::new(name.clone(), 0, 1);
            f.native = Some(nid);
            f.bound_generator = Some(g.clone());
            return Ok(Value::function(f));
        }
        return Ok(Value::undefined());
    }
    // Primitive string: `.length` and index access.
    if let ValueData::String(s) = obj.data() {
        if name == "length" {
            return Ok(Value::integer(s.chars().count() as i32));
        }
        if let Ok(i) = name.parse::<usize>() {
            if let Some(c) = s.as_str().chars().nth(i) {
                return Ok(Value::string(c.to_string()));
            }
        }
        return Ok(Value::undefined());
    }
    // Objects (incl. arrays): dictionary lookup + proto chain.
    let Some(handle) = obj_as_object(obj) else {
        return Ok(Value::undefined());
    };
    let mut cur = Some(handle.clone());
    while let Some(h) = cur {
        let b = h.borrow();
        if let Some(namespace) = &b.module_namespace {
            return match namespace.get(&name) {
                Some(cell) => cell.get(),
                None => Ok(Value::undefined()),
            };
        }
        if let Some(desc) = b.properties.get(&name) {
            return Ok(match desc {
                js_runtime::object::PropertyDescriptor::Data { value, .. } => value.clone(),
                js_runtime::object::PropertyDescriptor::Accessor { get, .. } => {
                    // Accessors aren't invoked in the milestone subset.
                    get.clone().unwrap_or_else(Value::undefined)
                }
            });
        }
        cur = b.proto.as_ref().and_then(|v| obj_as_object(v)).cloned();
    }
    Ok(Value::undefined())
}

fn is_object_value(value: &Value) -> bool {
    matches!(
        value.data(),
        ValueData::Object(_) | ValueData::Function(_) | ValueData::Generator(_)
    )
}

/// ECMAScript `HasProperty`: own lookup followed by the prototype chain.
pub(crate) fn has_property(obj: &Value, key: &Value) -> bool {
    if let ValueData::Symbol(symbol) = key.data() {
        let Some(handle) = obj_as_object(obj) else {
            return false;
        };
        let mut current = Some(handle.clone());
        while let Some(object) = current {
            let data = object.borrow();
            if data.symbol_properties.contains_key(&symbol.id) {
                return true;
            }
            current = data.proto.as_ref().and_then(obj_as_object).cloned();
        }
        return false;
    }
    let name = prop_name(key);
    if crate::builtins::native_static_id(obj, &name).is_some()
        || crate::builtins::builtin_method_id(obj, &name).is_some()
    {
        return true;
    }
    let Some(handle) = obj_as_object(obj) else {
        return false;
    };
    let mut current = Some(handle.clone());
    while let Some(object) = current {
        let data = object.borrow();
        if data
            .module_namespace
            .as_ref()
            .is_some_and(|namespace| namespace.contains_key(&name))
            || data.properties.contains_key(&name)
        {
            return true;
        }
        current = data.proto.as_ref().and_then(obj_as_object).cloned();
    }
    false
}

/// Delete an own property, respecting its `[[Configurable]]` attribute.
pub(crate) fn delete_property(obj: &Value, key: &Value) -> bool {
    let Some(handle) = obj_as_object(obj) else {
        return true;
    };
    if let ValueData::Symbol(symbol) = key.data() {
        let configurable = match handle.borrow().symbol_properties.get(&symbol.id) {
            Some(js_runtime::object::PropertyDescriptor::Data { attr, .. })
            | Some(js_runtime::object::PropertyDescriptor::Accessor { attr, .. }) => {
                attr.configurable
            }
            None => return true,
        };
        if configurable {
            handle.borrow_mut().symbol_properties.remove(&symbol.id);
            return true;
        }
        return false;
    }
    let name = prop_name(key);
    let configurable = {
        let data = handle.borrow();
        if let Some(namespace) = &data.module_namespace {
            return !namespace.contains_key(&name);
        }
        match data.properties.get(&name) {
            Some(js_runtime::object::PropertyDescriptor::Data { attr, .. })
            | Some(js_runtime::object::PropertyDescriptor::Accessor { attr, .. }) => {
                attr.configurable
            }
            None => return true,
        }
    };
    if configurable {
        handle.borrow_mut().properties.remove(&name);
        true
    } else {
        false
    }
}

fn type_error(message: &str) -> Value {
    crate::builtins::error_ctor(&Value::undefined(), &[Value::string(message)], "TypeError")
}

fn constant_string(module: &BytecodeModule, index: u16) -> String {
    match module.constants.get(index).data() {
        ValueData::String(value) => value.as_str().to_string(),
        _ => String::new(),
    }
}

fn create_template_object(site: &js_bytecode::module::TemplateSite) -> Value {
    use js_runtime::object::{Attribute, ObjectData, PropertyDescriptor};

    let element_attributes = Attribute {
        writable: false,
        enumerable: true,
        configurable: false,
    };
    let fixed_attributes = Attribute::read_only();
    let cooked = ObjectData::new_handle();
    let raw = ObjectData::new_handle();
    {
        let mut object = raw.borrow_mut();
        object.class = "Array";
        object.is_exotic_array = true;
        object.non_extensible = true;
        for (index, value) in site.raw.iter().enumerate() {
            object.properties.insert(
                index.to_string(),
                PropertyDescriptor::Data {
                    value: Value::string(value.as_str()),
                    attr: element_attributes,
                },
            );
        }
        object.properties.insert(
            "length".into(),
            PropertyDescriptor::Data {
                value: Value::integer(site.raw.len() as i32),
                attr: fixed_attributes,
            },
        );
    }
    {
        let mut object = cooked.borrow_mut();
        object.class = "Array";
        object.is_exotic_array = true;
        object.non_extensible = true;
        for (index, value) in site.cooked.iter().enumerate() {
            object.properties.insert(
                index.to_string(),
                PropertyDescriptor::Data {
                    value: value
                        .as_deref()
                        .map(Value::string)
                        .unwrap_or_else(Value::undefined),
                    attr: element_attributes,
                },
            );
        }
        object.properties.insert(
            "length".into(),
            PropertyDescriptor::Data {
                value: Value::integer(site.cooked.len() as i32),
                attr: fixed_attributes,
            },
        );
        object.properties.insert(
            "raw".into(),
            PropertyDescriptor::Data {
                value: Value::object(raw),
                attr: fixed_attributes,
            },
        );
    }
    Value::object(cooked)
}

fn type_error_named(name: &str, message: &str) -> Value {
    crate::builtins::error_ctor(&Value::undefined(), &[Value::string(message)], name)
}

/// `obj[key] = value` for object/array targets.
pub(crate) fn set_property(obj: &Value, key: &Value, value: Value) {
    let _ = set_property_checked(obj, key, value);
}

fn define_accessor(object: &Value, key: &Value, function: Value, getter: bool) {
    let Some(handle) = obj_as_object(object) else {
        return;
    };
    let name = prop_name(key);
    let mut data = handle.borrow_mut();
    let (mut get, mut set) = match data.properties.remove(&name) {
        Some(js_runtime::object::PropertyDescriptor::Accessor { get, set, .. }) => (get, set),
        _ => (None, None),
    };
    if getter {
        get = Some(function);
    } else {
        set = Some(function);
    }
    data.properties.insert(
        name,
        js_runtime::object::PropertyDescriptor::Accessor {
            get,
            set,
            attr: js_runtime::object::Attribute::writable(),
        },
    );
}

fn define_private_element(
    object: &Value,
    name: js_runtime::object::PrivateName,
    value: Value,
    opcode: Opcode,
) -> Result<(), InterpError> {
    let handle = obj_as_object(object)
        .cloned()
        .ok_or_else(|| InterpError::Throw(type_error("private element base is not an object")))?;
    let mut data = handle.borrow_mut();
    let templates = matches!(
        opcode,
        Opcode::DefinePrivateMethodTemplate
            | Opcode::DefinePrivateGetterTemplate
            | Opcode::DefinePrivateSetterTemplate
    );
    if !templates && data.non_extensible && !data.private_elements.contains_key(&name) {
        return Err(InterpError::Throw(type_error(
            "private element cannot be added to a non-extensible object",
        )));
    }
    let descriptor = match opcode {
        Opcode::DefinePrivate => PropertyDescriptor::Data {
            value,
            attr: js_runtime::object::Attribute {
                writable: true,
                enumerable: false,
                configurable: false,
            },
        },
        Opcode::DefinePrivateMethod | Opcode::DefinePrivateMethodTemplate => {
            PropertyDescriptor::Data {
                value,
                attr: js_runtime::object::Attribute::read_only(),
            }
        }
        Opcode::DefinePrivateGetter
        | Opcode::DefinePrivateSetter
        | Opcode::DefinePrivateGetterTemplate
        | Opcode::DefinePrivateSetterTemplate => {
            let existing = if templates {
                data.private_instance_elements.remove(&name)
            } else {
                data.private_elements.remove(&name)
            };
            let (mut get, mut set) = match existing {
                Some(PropertyDescriptor::Accessor { get, set, .. }) => (get, set),
                Some(previous) => {
                    if templates {
                        data.private_instance_elements.insert(name, previous);
                    } else {
                        data.private_elements.insert(name, previous);
                    }
                    return Err(InterpError::Throw(type_error(
                        "private element is already declared",
                    )));
                }
                None => (None, None),
            };
            if matches!(
                opcode,
                Opcode::DefinePrivateGetter | Opcode::DefinePrivateGetterTemplate
            ) {
                get = Some(value);
            } else {
                set = Some(value);
            }
            PropertyDescriptor::Accessor {
                get,
                set,
                attr: js_runtime::object::Attribute::read_only(),
            }
        }
        _ => unreachable!("non-private-definition opcode"),
    };
    let previous = if matches!(
        opcode,
        Opcode::DefinePrivateMethodTemplate
            | Opcode::DefinePrivateGetterTemplate
            | Opcode::DefinePrivateSetterTemplate
    ) {
        data.private_instance_elements.insert(name, descriptor)
    } else {
        data.private_elements.insert(name, descriptor)
    };
    if previous.is_some() {
        return Err(InterpError::Throw(type_error(
            "private element is already declared",
        )));
    }
    Ok(())
}

fn install_private_elements(
    object: &js_runtime::object::JsObject,
    elements: &HashMap<js_runtime::object::PrivateName, PropertyDescriptor>,
) -> Result<(), InterpError> {
    let mut data = object.borrow_mut();
    if data.non_extensible && !elements.is_empty() {
        return Err(InterpError::Throw(type_error(
            "private elements cannot be added to a non-extensible object",
        )));
    }
    if elements
        .keys()
        .any(|name| data.private_elements.contains_key(name))
    {
        return Err(InterpError::Throw(type_error(
            "private element is already installed on this object",
        )));
    }
    data.private_elements.extend(elements.clone());
    Ok(())
}

fn set_constructor_chain(object: &Value, constructor: &Value) {
    fn collect(value: &Value, identities: &mut Vec<js_runtime::object::ConstructorIdentity>) {
        let Some(function) = value.as_function() else {
            return;
        };
        let identity = js_runtime::object::ConstructorIdentity {
            module_index: function.module_index,
            function_id: function.id,
            native_id: function.native,
        };
        if !identities.contains(&identity) {
            identities.push(identity);
        }
        if let Some(superclass) = &function.superclass {
            collect(superclass, identities);
        }
    }

    let ValueData::Object(object) = object.data() else {
        return;
    };
    collect(constructor, &mut object.borrow_mut().constructor_chain);
}

fn copy_data_properties(target: &Value, source: &Value) {
    let (Some(target), Some(source)) = (obj_as_object(target), obj_as_object(source)) else {
        return;
    };
    let properties: Vec<_> = source
        .borrow()
        .properties
        .iter()
        .filter_map(|(name, descriptor)| match descriptor {
            js_runtime::object::PropertyDescriptor::Data { value, attr } if attr.enumerable => {
                Some((name.clone(), value.clone()))
            }
            _ => None,
        })
        .collect();
    let mut target = target.borrow_mut();
    for (name, value) in properties {
        target
            .properties
            .insert(name, js_runtime::object::PropertyDescriptor::data(value));
    }
}

pub(crate) fn set_property_checked(obj: &Value, key: &Value, value: Value) -> bool {
    let Some(handle) = obj_as_object(obj) else {
        return true; // silently ignore writes to primitives.
    };
    let name = prop_name(key);
    if handle.borrow().module_namespace.is_some() {
        return false;
    }
    if let ValueData::Symbol(symbol) = key.data() {
        if handle.borrow().non_extensible
            && !handle.borrow().symbol_properties.contains_key(&symbol.id)
        {
            return false;
        }
        let mut data = handle.borrow_mut();
        if let Some(PropertyDescriptor::Data { value: slot, attr }) =
            data.symbol_properties.get_mut(&symbol.id)
        {
            if !attr.writable {
                return false;
            }
            *slot = value;
            return true;
        }
        data.symbol_properties.insert(
            symbol.id,
            js_runtime::object::PropertyDescriptor::data(value),
        );
        return true;
    }
    let is_array = handle.borrow().is_exotic_array;
    let new_len = if is_array {
        name.parse::<usize>().ok()
    } else {
        None
    };
    let mut b = handle.borrow_mut();
    if b.non_extensible && !b.properties.contains_key(&name) {
        return false;
    }
    if let Some(descriptor) = b.properties.get_mut(&name) {
        match descriptor {
            PropertyDescriptor::Data { value: slot, attr } if attr.writable => *slot = value,
            PropertyDescriptor::Data { .. } | PropertyDescriptor::Accessor { .. } => return false,
        }
    } else {
        b.properties.insert(
            name.clone(),
            js_runtime::object::PropertyDescriptor::data(value),
        );
    }
    if let Some(idx) = new_len {
        let cur = b
            .properties
            .get("length")
            .and_then(|d| match d {
                js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
                _ => None,
            })
            .and_then(|v| match v.data() {
                ValueData::Integer(i) => Some(*i as usize),
                ValueData::Number(n) => Some(*n as usize),
                _ => None,
            })
            .unwrap_or(0);
        if idx >= cur {
            b.properties.insert(
                "length".to_string(),
                js_runtime::object::PropertyDescriptor::data(Value::integer((idx + 1) as i32)),
            );
        }
    }
    true
}

fn define_own_property(
    object: &js_runtime::object::JsObject,
    key: &Value,
    descriptor: PropertyDescriptor,
) -> bool {
    let mut data = object.borrow_mut();
    let current = match key.data() {
        ValueData::Symbol(symbol) => data.symbol_properties.get(&symbol.id),
        _ => data.properties.get(&prop_name(key)),
    };
    if current.is_none() && data.non_extensible {
        return false;
    }
    if let Some(current) = current {
        let current_attr = match current {
            PropertyDescriptor::Data { attr, .. } | PropertyDescriptor::Accessor { attr, .. } => {
                *attr
            }
        };
        let new_attr = match &descriptor {
            PropertyDescriptor::Data { attr, .. } | PropertyDescriptor::Accessor { attr, .. } => {
                *attr
            }
        };
        if !current_attr.configurable
            && (new_attr.configurable || new_attr.enumerable != current_attr.enumerable)
        {
            return false;
        }
        if let PropertyDescriptor::Data {
            value: current_value,
            attr: current_attr,
        } = current
        {
            if !current_attr.configurable && !current_attr.writable {
                let PropertyDescriptor::Data {
                    value: new_value,
                    attr: new_attr,
                } = &descriptor
                else {
                    return false;
                };
                if new_attr.writable || !same_value_runtime(current_value, new_value) {
                    return false;
                }
            }
        } else if !current_attr.configurable
            && matches!(descriptor, PropertyDescriptor::Data { .. })
        {
            return false;
        }
    }
    match key.data() {
        ValueData::Symbol(symbol) => {
            data.symbol_properties.insert(symbol.id, descriptor);
        }
        _ => {
            data.properties.insert(prop_name(key), descriptor);
        }
    }
    true
}

fn property_descriptor_value(descriptor: &PropertyDescriptor) -> Value {
    let value = Value::object(js_runtime::object::ObjectData::new_handle());
    match descriptor {
        PropertyDescriptor::Data {
            value: property_value,
            attr,
        } => {
            set_property(&value, &Value::string("value"), property_value.clone());
            set_property(
                &value,
                &Value::string("writable"),
                Value::boolean(attr.writable),
            );
            set_property(
                &value,
                &Value::string("enumerable"),
                Value::boolean(attr.enumerable),
            );
            set_property(
                &value,
                &Value::string("configurable"),
                Value::boolean(attr.configurable),
            );
        }
        PropertyDescriptor::Accessor { get, set, attr } => {
            set_property(
                &value,
                &Value::string("get"),
                get.clone().unwrap_or_else(Value::undefined),
            );
            set_property(
                &value,
                &Value::string("set"),
                set.clone().unwrap_or_else(Value::undefined),
            );
            set_property(
                &value,
                &Value::string("enumerable"),
                Value::boolean(attr.enumerable),
            );
            set_property(
                &value,
                &Value::string("configurable"),
                Value::boolean(attr.configurable),
            );
        }
    }
    value
}

fn binding_error_value(error: js_runtime::value::BindingError) -> InterpError {
    let value = match error {
        js_runtime::value::BindingError::Uninitialized => crate::builtins::error_ctor(
            &Value::undefined(),
            &[Value::string("cannot access binding before initialization")],
            "ReferenceError",
        ),
        js_runtime::value::BindingError::Immutable => type_error("assignment to immutable binding"),
    };
    InterpError::Throw(value)
}

/// Build an iterator value for an iterable. Generators are their own iterator;
/// arrays and strings get an index-cursor object.
fn make_iterator(iterable: &Value) -> Value {
    if iterable.is_generator() {
        return iterable.clone();
    }
    let is_indexable = matches!(
        iterable.data(),
        ValueData::Object(o) if o.borrow().is_exotic_array)
        || matches!(iterable.data(), ValueData::String(_));
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.class = "ArrayIterator";
        b.properties.insert(
            IT_SRC.to_string(),
            js_runtime::object::PropertyDescriptor::data(if is_indexable {
                iterable.clone()
            } else {
                Value::undefined()
            }),
        );
        b.properties.insert(
            IT_IDX.to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::integer(0)),
        );
    }
    Value::object(o)
}

fn iterator_record_value(record: IteratorRecord) -> Value {
    let object = js_runtime::object::ObjectData::new_handle();
    {
        let mut data = object.borrow_mut();
        data.class = "IteratorRecord";
        data.iterator_record = Some(record);
    }
    Value::object(object)
}

/// Step an array-iterator object once: read `__it_src`/`__it_idx`, produce the
/// next `{value, done}` result, and bump the index.
fn step_array_iterator(it: &Value) -> Value {
    let src = get_property(it, &Value::string(IT_SRC));
    let idx = get_property(it, &Value::string(IT_IDX));
    let len = get_property(&src, &Value::string("length"));
    let i = match idx.data() {
        ValueData::Integer(i) => *i,
        ValueData::Number(n) => *n as i32,
        _ => -1,
    };
    let l = match len.data() {
        ValueData::Integer(i) => *i,
        ValueData::Number(n) => *n as i32,
        _ => 0,
    };
    if i < l {
        let value = get_property(&src, &Value::string(i.to_string()));
        set_property(it, &Value::string(IT_IDX), Value::integer(i + 1));
        iter_result(value, false)
    } else {
        iter_result(Value::undefined(), true)
    }
}

/// Construct an array-object from a vec of values (sets numeric indices + length).
///
/// `array_proto` is the realm's `%ArrayPrototype%`, threaded explicitly so the
/// result links to the *current* realm's prototypes without any thread-local
/// (which would break multi-realm isolation on one thread). Callers with an
/// interpreter pass [`Interpreter::array_prototype`]; the few creation paths
/// that have no realm (host bootstrapping) pass `None`.
pub(crate) fn make_array(vals: Vec<Value>, array_proto: Option<Value>) -> Value {
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.class = "Array";
        b.is_exotic_array = true;
        b.proto = array_proto;
        for (i, v) in vals.iter().enumerate() {
            b.properties.insert(
                i.to_string(),
                js_runtime::object::PropertyDescriptor::data(v.clone()),
            );
        }
        b.properties.insert(
            "length".to_string(),
            js_runtime::object::PropertyDescriptor::data(Value::integer(vals.len() as i32)),
        );
    }
    Value::object(o)
}

/// Read an array/object's numeric length (0 for non-arrays).
pub(crate) fn array_len(arr: &Value) -> usize {
    match arr.data() {
        ValueData::Object(o) => {
            let b = o.borrow();
            b.properties
                .get("length")
                .and_then(|d| match d {
                    js_runtime::object::PropertyDescriptor::Data { value, .. } => {
                        Some(value.clone())
                    }
                    _ => None,
                })
                .and_then(|v| match v.data() {
                    ValueData::Integer(i) => Some(*i as usize),
                    ValueData::Number(n) => Some(*n as usize),
                    _ => None,
                })
                .unwrap_or(0)
        }
        ValueData::String(s) => s.chars().count(),
        _ => 0,
    }
}

/// Read element `i` of an array (undefined if missing).
pub(crate) fn array_get(arr: &Value, i: usize) -> Value {
    get_property(arr, &Value::string(i.to_string()))
}

pub(crate) fn array_append(arr: &Value, v: Value) {
    let Some(handle) = obj_as_object(arr) else {
        return;
    };
    let mut b = handle.borrow_mut();
    let len = b
        .properties
        .get("length")
        .and_then(|d| match d {
            js_runtime::object::PropertyDescriptor::Data { value, .. } => Some(value.clone()),
            _ => None,
        })
        .and_then(|v| match v.data() {
            ValueData::Integer(i) => Some(*i as usize),
            ValueData::Number(n) => Some(*n as usize),
            _ => None,
        })
        .unwrap_or(0);
    b.properties.insert(
        len.to_string(),
        js_runtime::object::PropertyDescriptor::data(v),
    );
    b.properties.insert(
        "length".to_string(),
        js_runtime::object::PropertyDescriptor::data(Value::integer((len + 1) as i32)),
    );
}

/// Borrow the `JsObject` handle from an object Value, if it is one.
pub(crate) fn obj_as_object(v: &Value) -> Option<&js_runtime::object::JsObject> {
    match v.data() {
        ValueData::Object(o) => Some(o),
        ValueData::Function(function) => Some(&function.object),
        _ => None,
    }
}

fn same_value_runtime(left: &Value, right: &Value) -> bool {
    match (left.data(), right.data()) {
        (ValueData::Number(left), ValueData::Number(right)) => {
            (left.is_nan() && right.is_nan()) || left.to_bits() == right.to_bits()
        }
        _ => eq_strict(left.clone(), right.clone()),
    }
}

/// Run a module end-to-end, lifting structured runtime failures into the
/// legacy `DiagResult` shape. New host integrations should prefer
/// [`Interpreter::run_module_report`] so JavaScript exceptions remain distinct.
pub fn run(module: &BytecodeModule, ctx: RealmContext) -> DiagResult<Value> {
    match Interpreter::new(ctx).run_module_report(module) {
        Ok(v) => Ok(v),
        Err(RuntimeError::Fault(error)) => {
            let mut diagnostic = js_diagnostics::Diagnostic::new(
                js_diagnostics::Severity::Bug,
                error.span(),
                error.message,
            )
            .with_phase(js_diagnostics::DiagnosticPhase::Internal)
            .with_code("JS-INTERNAL");
            for frame in error.stack.iter().skip(1) {
                diagnostic =
                    diagnostic.with_note(frame.span, format!("called from `{}`", frame.function));
            }
            Err(vec![diagnostic])
        }
        Err(RuntimeError::Exception(error)) => {
            let mut diagnostic = js_diagnostics::Diagnostic::error(
                error.span(),
                format!("Uncaught {}", to_string(&error.value)),
            )
            .with_phase(js_diagnostics::DiagnosticPhase::Runtime)
            .with_code("JS-RUNTIME");
            for frame in error.stack.iter().skip(1) {
                diagnostic =
                    diagnostic.with_note(frame.span, format!("called from `{}`", frame.function));
            }
            Err(vec![diagnostic])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_undefined_for_empty_module() {
        let module = js_bytecode::compile_program(&js_syntax::ast::Program::new(
            js_syntax::Span::DUMMY,
            js_syntax::ast::ProgramKind::Script,
            vec![],
        ))
        .expect("compile");
        let v = Interpreter::fresh().run_module(&module).expect("run");
        assert!(v.is_undefined());
    }
}
