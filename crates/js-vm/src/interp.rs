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
use js_runtime::value::{GeneratorState, JsFunction, Value, ValueData};
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
/// paused generator with `arg` (the value passed to `.next(arg)`).
pub enum NativeResult {
    Value(Value),
    ResumeGenerator(Rc<RefCell<GeneratorState>>, Value),
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
        interp: &mut Interpreter,
        modules: &BytecodeGraph<'_>,
        this: Value,
        f: &JsFunction,
        args: Vec<Value>,
    ) -> Result<NativeResult, InterpError>;
}

struct GenNext;
struct GenReturn;
struct GenThrow;

impl NativeFn for GenThrow {
    fn call(
        &self,
        _interp: &mut Interpreter,
        _modules: &BytecodeGraph<'_>,
        _this: Value,
        f: &JsFunction,
        _args: Vec<Value>,
    ) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        gen.borrow_mut().done = true;
        Ok(NativeResult::Value(iter_result(Value::undefined(), true)))
    }
}

impl NativeFn for GenNext {
    fn call(
        &self,
        _interp: &mut Interpreter,
        _modules: &BytecodeGraph<'_>,
        _this: Value,
        f: &JsFunction,
        args: Vec<Value>,
    ) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let arg = args.into_iter().next().unwrap_or_else(Value::undefined);
        Ok(NativeResult::ResumeGenerator(gen, arg))
    }
}

impl NativeFn for GenReturn {
    fn call(
        &self,
        _interp: &mut Interpreter,
        _modules: &BytecodeGraph<'_>,
        _this: Value,
        f: &JsFunction,
        args: Vec<Value>,
    ) -> Result<NativeResult, InterpError> {
        let gen = f.bound_generator.clone().ok_or_else(|| {
            InterpError::Internal("generator method has no bound generator".into())
        })?;
        let value = args.into_iter().next().unwrap_or_else(Value::undefined);
        gen.borrow_mut().done = true;
        Ok(NativeResult::Value(iter_result(value, true)))
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
    dynamic_imports: Option<Vec<HashMap<String, Result<usize, String>>>>,
    dynamic_import_requests: VecDeque<DynamicImportRequest>,
    deferred_modules: Option<DeferredModuleGraph>,
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
        };
        // Install the global builtins (console, Math, Object, JSON, parseInt, …).
        {
            let mut realm = interp.ctx.realm.borrow_mut();
            crate::builtins::install_globals(&mut realm.globals);
            let global_this = Value::object(realm.global_object.clone());
            realm.globals.insert("globalThis".into(), global_this);
        }
        interp
    }

    /// Construct an interpreter with a fresh realm.
    pub fn fresh() -> Interpreter {
        Interpreter::new(RealmContext::fresh())
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

    pub fn configure_dynamic_imports(
        &mut self,
        resolutions: Vec<HashMap<String, Result<usize, String>>>,
    ) {
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

    pub(crate) fn mark_test262_done(&mut self) {
        self.ctx.realm.borrow_mut().test262_done_called = true;
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
        self.frames.push(CallFrame::with_locals(
            module_index,
            0,
            locals,
            module.main.span,
        ));
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
                        .map(|frame| runtime_frame(modules, frame))
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
                    if func_ref(modules[frame.module_index], frame.func_index).is_async {
                        let promise = crate::builtins::promise_rejected(thrown);
                        self.frames.pop();
                        if let Some(caller) = self.frames.last_mut() {
                            caller.stack.push(promise);
                            return Ok(());
                        }
                        return Err(InterpError::Internal(
                            "async function escaped without a caller frame".into(),
                        ));
                    }
                    self.error_trace.push(runtime_frame(modules, frame));
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
        let module_index = self.frames.last().unwrap().module_index;
        let module = modules.get(module_index).copied().ok_or_else(|| {
            InterpError::Internal(format!(
                "frame refers to missing bytecode module {module_index}"
            ))
        })?;
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
                self.top().stack.push(make_iterator(&v));
            }
            Opcode::IterNext => {
                let it = self.top().stack.pop();
                if it.is_generator() {
                    // Drive one `.next()` via the call machinery; the
                    // iterator-result lands on this frame's stack when the
                    // generator yields/returns (or synchronously if done).
                    let next_fn = get_property(&it, &Value::string("next"));
                    self.invoke(modules, next_fn, Vec::new(), it, false);
                } else {
                    // Array-iterator object: step by index synchronously.
                    let result = step_array_iterator(&it);
                    self.top().stack.push(result);
                }
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
            Opcode::LdaThis => {
                let v = self.current_this();
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
                class.as_function_mut().unwrap().superclass = Some(Box::new(superclass));
                self.top().stack.push(class);
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
                let v = self
                    .ctx
                    .realm
                    .borrow()
                    .globals
                    .get(&name)
                    .cloned()
                    .unwrap_or_default();
                self.top().stack.push(v);
            }
            Opcode::SetGlobal => {
                let v = self.top().stack.pop();
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str().to_string(),
                    _ => String::new(),
                };
                self.ctx.realm.borrow_mut().globals.insert(name, v);
            }
            Opcode::DeleteGlobal => {
                let name = match module.constants.get(ins.operand).data() {
                    ValueData::String(s) => s.as_str().to_string(),
                    _ => String::new(),
                };
                self.ctx.realm.borrow_mut().globals.remove(&name);
                self.top().stack.push(Value::boolean(true));
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
            Opcode::Return => {
                let (ret, was_construct, this_obj, gen) = {
                    let f = self.frames.last_mut().unwrap();
                    (
                        f.stack.pop(),
                        f.is_construct,
                        f.this.clone(),
                        f.generator.clone(),
                    )
                };
                // A generator body completing: mark done, return {value, done:true}.
                if let Some(g) = gen {
                    g.borrow_mut().done = true;
                    self.frames.pop();
                    if self.frames.is_empty() {
                        return Ok(Step::Done(Value::undefined()));
                    }
                    self.top().stack.push(iter_result(ret, true));
                    return Ok(Step::More);
                }
                let is_async = {
                    let frame = self.frames.last().unwrap();
                    func_ref(modules[frame.module_index], frame.func_index).is_async
                };
                if self.frames.len() == 1 {
                    self.drain_jobs(modules)?;
                }
                self.frames.pop();
                let ret = if was_construct && !ret.is_object() {
                    this_obj
                } else if is_async {
                    crate::builtins::promise_resolved(self, modules, ret)?
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
                // Suspend the current generator frame: pop the yielded
                // value, save the frame back into its generator, then pop
                // the frame and hand {value, done:false} to the caller.
                let gen = self.frames.last().unwrap().generator.clone();
                let gen = gen.expect("`yield` outside a generator frame");
                let yielded = self.frames.last_mut().unwrap().stack.pop();
                {
                    let mut s = gen.borrow_mut();
                    let f = self.frames.last().unwrap();
                    s.pc = f.pc;
                    s.locals = self.frames.last_mut().unwrap().locals.split_off(0);
                    let depth = self.frames.last().unwrap().stack.depth();
                    let mut v: Vec<Value> = (0..depth)
                        .map(|_| self.frames.last_mut().unwrap().stack.pop())
                        .collect();
                    v.reverse();
                    s.stack = v;
                    s.upvalues = self.frames.last_mut().unwrap().upvalues.split_off(0);
                    s.this = self.frames.last_mut().unwrap().this.clone();
                    s.captured_this = self.frames.last_mut().unwrap().captured_this.take();
                    s.done = false;
                }
                self.frames.pop();
                if self.frames.is_empty() {
                    // `yield` at the top of the script (no caller) — drop it.
                    return Ok(Step::Done(Value::undefined()));
                }
                self.top().stack.push(iter_result(yielded, false));
            }
            Opcode::Await => {
                let awaited = self.top().stack.pop();
                let awaited = crate::builtins::promise_resolved(self, modules, awaited)?;
                if self.frames.len() == 1 && self.top().func_index == 0 {
                    return Ok(Step::Suspend(awaited));
                }
                self.drain_jobs(modules)?;
                match crate::builtins::promise_result(&awaited) {
                    Some(crate::builtins::AwaitedPromise::Fulfilled(value)) => {
                        self.top().stack.push(value)
                    }
                    Some(crate::builtins::AwaitedPromise::Rejected(value)) => {
                        return Err(InterpError::Throw(value));
                    }
                    Some(crate::builtins::AwaitedPromise::Pending) => {
                        return Err(InterpError::Internal(
                            "awaited Promise is still pending after the job checkpoint".into(),
                        ));
                    }
                    None => unreachable!("PromiseResolve always returns a Promise"),
                }
            }

            // ---- calls ----
            Opcode::Call => {
                let (callee, args) = pop_args(self, ins.operand);
                self.invoke(modules, callee, args, Value::undefined(), false);
            }
            Opcode::CallMethod => {
                // Stack: [obj, fn, args...]
                let (args, this, callee) = {
                    let n = ins.operand as usize;
                    let frame = self.frames.last_mut().unwrap();
                    let mut a: Vec<Value> = (0..n).map(|_| frame.stack.pop()).collect();
                    a.reverse();
                    let callee = frame.stack.pop();
                    let this = frame.stack.pop();
                    (a, this, callee)
                };
                self.invoke(modules, callee, args, this, false);
            }
            Opcode::CallSuper => {
                let args = {
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
                let base = self.current_this();
                let result = self.call_value_mode(modules, superclass, args, base.clone(), true)?;
                let frame = self.frames.last_mut().unwrap();
                frame.super_base = Some(base.clone());
                frame.this = if result.is_object() {
                    result.clone()
                } else {
                    base
                };
                frame.stack.push(result);
            }
            Opcode::SetSuperProp => {
                let key = self.top().stack.pop();
                let value = self.top().stack.pop();
                self.set_super_property(modules, &key, value)?;
            }
            Opcode::NewObject => {
                let o = js_runtime::object::ObjectData::new_handle();
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
            Opcode::GetProp => {
                let key = self.top().stack.pop();
                let obj = self.top().stack.pop();
                self.ensure_deferred_namespace(modules, &obj)?;
                let value = self.get_property_value(modules, &obj, &key)?;
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
                if !set_property_checked(&obj, &key, value) {
                    return Err(InterpError::Throw(type_error(
                        "cannot assign to a module namespace property",
                    )));
                }
            }
            Opcode::DefineGetter | Opcode::DefineSetter => {
                let key = self.top().stack.pop();
                let object = self.top().stack.pop();
                let function = self.top().stack.pop();
                define_accessor(&object, &key, function, ins.op == Opcode::DefineGetter);
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
            Opcode::New => {
                let (callee, args) = pop_args(self, ins.operand);
                if let Some(value) = crate::builtins::construct_builtin(&callee, &args) {
                    set_constructor_chain(&value, &callee);
                    self.top().stack.push(value);
                    return Ok(Step::More);
                }
                // Construct a fresh object bound as `this`.
                let this = Value::object(js_runtime::object::ObjectData::new_handle());
                set_constructor_chain(&this, &callee);
                self.invoke(modules, callee, args, this, true);
            }
            Opcode::DynamicImport => {
                let specifier = to_string(&self.top().stack.pop());
                let module_index = self.top().module_index;
                let resolution = self
                    .dynamic_imports
                    .as_ref()
                    .and_then(|modules| modules.get(module_index))
                    .and_then(|requests| requests.get(&specifier))
                    .cloned()
                    .unwrap_or_else(|| {
                        Err(format!(
                            "dynamic import `{specifier}` was not resolved by the module host"
                        ))
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

    fn get_property_value(
        &mut self,
        modules: &BytecodeGraph<'_>,
        object: &Value,
        key: &Value,
    ) -> Result<Value, InterpError> {
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
                                    Some(getter) if getter.is_function() => {
                                        self.call_value(modules, getter, Vec::new(), object.clone())
                                    }
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
                            self.call_value(modules, getter, Vec::new(), object.clone())
                        }
                        _ => Ok(Value::undefined()),
                    },
                };
            }
            current = prototype.as_ref().and_then(obj_as_object).cloned();
        }
        get_property_checked(object, key).map_err(binding_error_value)
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
            let module = modules.get(module_index).copied().ok_or_else(|| {
                InterpError::Internal(format!(
                    "PromiseJob callback refers to missing module {module_index}"
                ))
            })?;
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
        let (base, receiver) = {
            let frame = self.frames.last().unwrap();
            (
                frame
                    .super_base
                    .clone()
                    .unwrap_or_else(|| frame.this.clone()),
                frame.this.clone(),
            )
        };
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

    pub(crate) fn enqueue_promise_job(&mut self, job: PromiseJob) {
        self.jobs.push_back(job);
    }

    fn drain_jobs(&mut self, modules: &BytecodeGraph<'_>) -> Result<(), InterpError> {
        while let Some(job) = self.jobs.pop_front() {
            match job {
                PromiseJob::Reaction {
                    reaction,
                    argument,
                    rejected,
                } => {
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
            Some(new_cell(self.current_this()))
        } else {
            None
        };
        let mut f = JsFunction::new(func.name.clone(), id, func.param_count);
        f.module_index = module_index as u32;
        f.upvalues = upvalues;
        f.this_cell = this_cell;
        f.is_generator = func.is_generator;
        f
    }

    /// The `this` value for the current frame (ordinary frame `this`, or the
    /// arrow's lexically captured `this`).
    fn current_this(&self) -> Value {
        let frame = self.frames.last().unwrap();
        if let Some(c) = &frame.captured_this {
            return c.get().unwrap_or_else(|_| Value::undefined());
        }
        frame.this.clone()
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
                self.top().stack.push(if is_construct {
                    this
                } else {
                    Value::undefined()
                });
                return;
            }
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
            let result = unsafe { (&*nf_ptr).call(self, modules, this, &f, args) };
            match result {
                Ok(NativeResult::Value(v)) => {
                    self.error_trace = prior_trace;
                    self.top().stack.push(v)
                }
                Ok(NativeResult::ResumeGenerator(gen, arg)) => {
                    self.error_trace = prior_trace;
                    self.checkout_generator(gen, arg);
                }
                Err(e) => self.pending_err = Some(e),
            }
            return;
        }
        let module_index = f.module_index as usize;
        let Some(module) = modules.get(module_index).copied() else {
            self.pending_err = Some(InterpError::Internal(format!(
                "function refers to missing bytecode module {module_index}"
            )));
            return;
        };
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
        for i in 0..(param_count as usize).min(args.len()) {
            nf.locals[i] = new_cell(args[i].clone());
        }
        nf.upvalues = f.upvalues.clone();
        if f.this_cell.is_some() {
            nf.captured_this = f.this_cell.clone();
        } else {
            nf.this = this;
        }
        nf.is_construct = is_construct;
        nf.superclass = f.superclass.as_deref().cloned();
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
            this: if f.this_cell.is_some() {
                Value::undefined()
            } else {
                this
            },
            captured_this: f.this_cell.clone(),
            done: false,
            started: false,
        }
    }

    /// Resume a paused generator: check its frame state out into a live
    /// `CallFrame`, push the `.next(arg)` argument (for non-first resumes), and
    /// push the frame so the dispatch loop continues it.
    fn checkout_generator(&mut self, gen: Rc<RefCell<GeneratorState>>, arg: Value) {
        let (
            done,
            started,
            module_index,
            func_index,
            pc,
            locals,
            stack,
            upvalues,
            this,
            captured_this,
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
                std::mem::replace(&mut s.this, Value::undefined()),
                s.captured_this.take(),
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
        frame.this = this;
        frame.captured_this = captured_this;
        frame.generator = Some(gen);
        for v in stack {
            frame.stack.push(v);
        }
        if started {
            // The `.next(arg)` argument is the value of the `yield` expression.
            frame.stack.push(arg);
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

fn func_ref<'a>(module: &'a BytecodeModule, index: usize) -> &'a BytecodeFunction {
    if index == 0 {
        &module.main
    } else {
        &module.functions[index - 1]
    }
}

fn runtime_frame(modules: &BytecodeGraph<'_>, frame: &CallFrame) -> RuntimeFrame {
    let module = modules[frame.module_index];
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
        (Function(x), Function(y)) => x.id == y.id,
        (Symbol(x), Symbol(y)) => x.id == y.id,
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
    !is_falsy(v)
}

fn is_falsy(v: &Value) -> bool {
    match v.data() {
        ValueData::Undefined | ValueData::Null => true,
        ValueData::Boolean(b) => !b,
        ValueData::Integer(i) => *i == 0,
        ValueData::Number(n) => *n == 0.0 || n.is_nan(),
        ValueData::String(s) => s.is_empty(),
        _ => false,
    }
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
        handle.borrow_mut().symbol_properties.insert(
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
    b.properties
        .insert(name, js_runtime::object::PropertyDescriptor::data(value));
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

/// Append `v` to an array-object (sets the next numeric index, bumps `length`).
/// Construct an array-object from a vec of values (sets numeric indices + length).
pub(crate) fn make_array(vals: Vec<Value>) -> Value {
    let o = js_runtime::object::ObjectData::new_handle();
    {
        let mut b = o.borrow_mut();
        b.class = "Array";
        b.is_exotic_array = true;
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
fn obj_as_object(v: &Value) -> Option<&js_runtime::object::JsObject> {
    match v.data() {
        ValueData::Object(o) => Some(o),
        _ => None,
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
