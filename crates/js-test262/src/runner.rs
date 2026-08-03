//! The runner: walks a directory tree, classifies each test, runs the parser,
//! and aggregates results.

use crate::frontmatter::{FrontMatter, NegativePhase};
use serde::Serialize;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Variant {
    Sloppy,
    Strict,
    Module,
    Raw,
}

impl Variant {
    pub fn as_str(self) -> &'static str {
        match self {
            Variant::Sloppy => "sloppy",
            Variant::Strict => "strict",
            Variant::Module => "module",
            Variant::Raw => "raw",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sloppy" => Some(Variant::Sloppy),
            "strict" => Some(Variant::Strict),
            "module" => Some(Variant::Module),
            "raw" => Some(Variant::Raw),
            _ => None,
        }
    }

    fn source<'a>(self, src: &'a str) -> Cow<'a, str> {
        match self {
            Variant::Strict => Cow::Owned(format!("\"use strict\";\n{src}")),
            _ => Cow::Borrowed(src),
        }
    }
}

pub fn variants(fm: &FrontMatter) -> Vec<Variant> {
    if fm.flags.iter().any(|f| f == "module") {
        vec![Variant::Module]
    } else if fm.flags.iter().any(|f| f == "raw") {
        vec![Variant::Raw]
    } else if fm.flags.iter().any(|f| f == "onlyStrict") {
        vec![Variant::Strict]
    } else if fm.flags.iter().any(|f| f == "noStrict") {
        vec![Variant::Sloppy]
    } else {
        vec![Variant::Sloppy, Variant::Strict]
    }
}

/// What we expect the parser to do for a given test.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Expect {
    /// Source must parse without error.
    Ok,
    /// Source must be rejected by the parser.
    Err,
    /// Not applicable (e.g. early-error phase; we don't implement those).
    Skip,
}

/// The outcome for a single test.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Outcome {
    /// Expectation met.
    Pass,
    /// Expectation violated — an actionable result.
    Fail { reason: String },
    /// The parser or runner could not produce a meaningful conformance result.
    Incomplete { reason: String },
    /// Skipped because the case is outside the selected phase.
    Skip(String),
}

#[derive(Clone, Debug, Serialize)]
pub struct TestResult {
    pub path: PathBuf,
    pub variant: Variant,
    pub features: Vec<String>,
    pub expect: Expect,
    pub outcome: Outcome,
}

/// Aggregated statistics.
#[derive(Default, Debug, Serialize)]
pub struct Stats {
    pub files: usize,
    /// Number of strict/sloppy/module/raw variants that were judged.
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub incomplete: usize,
    pub skip: usize,
    /// False accepts: we returned Ok but the test demanded a parse error.
    pub false_accept: usize,
    /// False rejects on ExpectOk: we returned Err but parse success was required.
    pub false_reject: usize,
}

/// Walk `root`, expand each file into its Test262 variants, and run the parser.
pub fn run(root: &Path) -> (Vec<TestResult>, Stats) {
    let mut results = Vec::new();
    let mut stats = Stats::default();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_js(root, &mut files);
    files.sort();
    stats.files = files.len();

    for path in files {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                stats.skip += 1;
                stats.total += 1;
                results.push(TestResult {
                    path,
                    variant: Variant::Sloppy,
                    features: Vec::new(),
                    expect: Expect::Skip,
                    outcome: Outcome::Skip("unreadable".into()),
                });
                continue;
            }
        };
        let fm = match FrontMatter::parse_result(&src) {
            Ok(fm) => fm.unwrap_or_default(),
            Err(reason) => {
                stats.total += 1;
                stats.incomplete += 1;
                results.push(TestResult {
                    path,
                    variant: Variant::Sloppy,
                    features: Vec::new(),
                    expect: Expect::Skip,
                    outcome: Outcome::Incomplete {
                        reason: format!("metadata: {reason}"),
                    },
                });
                continue;
            }
        };
        let expect = classify(&fm);
        for variant in variants(&fm) {
            stats.total += 1;
            let outcome = run_parse_variant(&src, variant, expect, &mut stats);
            results.push(TestResult {
                path: path.clone(),
                variant,
                features: fm.features.clone(),
                expect,
                outcome,
            });
        }
    }

    (results, stats)
}

fn run_parse_variant(src: &str, variant: Variant, expect: Expect, stats: &mut Stats) -> Outcome {
    if expect == Expect::Skip {
        stats.skip += 1;
        return Outcome::Skip("not a parse-phase test".into());
    }
    let source = variant.source(src);
    let parse_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if variant == Variant::Module {
            js_parser::parse_module(&source)
        } else {
            js_parser::parse(&source)
        }
    }));
    let parse_ok = match parse_res {
        Ok(Ok(_)) => true,
        Ok(Err(diags)) => {
            let reason = diags
                .first()
                .map(|d| d.message.clone())
                .unwrap_or_else(|| "(no diagnostic)".into());
            return match expect {
                Expect::Err => {
                    stats.pass += 1;
                    Outcome::Pass
                }
                Expect::Ok => {
                    stats.fail += 1;
                    stats.false_reject += 1;
                    Outcome::Fail {
                        reason: format!("parse Err: {reason}"),
                    }
                }
                Expect::Skip => unreachable!(),
            };
        }
        Err(_) => {
            stats.incomplete += 1;
            return Outcome::Incomplete {
                reason: "parser PANICKED".into(),
            };
        }
    };
    debug_assert!(parse_ok);
    match expect {
        Expect::Ok => {
            stats.pass += 1;
            Outcome::Pass
        }
        Expect::Err => {
            stats.fail += 1;
            stats.false_accept += 1;
            Outcome::Fail {
                reason: "expected parse Err (invalid syntax), got Ok".into(),
            }
        }
        Expect::Skip => unreachable!(),
    }
}

fn classify(fm: &FrontMatter) -> Expect {
    match &fm.negative_phase {
        Some(NegativePhase::Parse | NegativePhase::Early) => Expect::Err,
        _ => Expect::Ok,
    }
}

fn collect_js(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            // Skip test262 fixture directories: anything literally named
            // "fixture" or hidden dirs.
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "fixture" || name.starts_with('.') {
                continue;
            }
            collect_js(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("js")
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("_FIXTURE.js"))
        {
            out.push(path);
        }
    }
}

// ---- runtime (execution) phase -------------------------------------------

/// A runtime-phase expectation derived from the frontmatter.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", content = "error", rename_all = "lowercase")]
pub enum RuntimeExpect {
    /// Run to completion without throwing.
    CleanRun,
    /// Throw an error whose `.name` matches this.
    Throws(String),
    /// A module must fail during resolution/linking with the named error type.
    ResolutionThrows(String),
}

/// Aggregated runtime statistics.
#[derive(Default, Debug, Serialize)]
pub struct RuntimeStats {
    pub files: usize,
    /// Number of expanded Test262 variants.
    pub total: usize,
    /// Tests that were actually executed (not skipped up front).
    pub executed: usize,
    pub pass: usize,
    /// Wrong outcome: threw when it shouldn't, didn't throw, or wrong type.
    pub fail: usize,
    /// The VM couldn't run it (unimplemented feature / VM bug). Not a real fail.
    pub incomplete: usize,
    pub skip: usize,
}

/// Per-test runtime outcome.
#[derive(Clone, Debug, Serialize)]
pub struct RuntimeResult {
    pub path: PathBuf,
    pub variant: Variant,
    pub features: Vec<String>,
    pub expect: Option<RuntimeExpect>,
    pub outcome: RuntimeOutcome,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", content = "reason", rename_all = "lowercase")]
pub enum RuntimeOutcome {
    Pass,
    Fail(String),
    Incomplete(String),
    Skip(String),
}

/// The harness helper files whose API we provide natively (`assert.js`, `sta.js`).
/// Tests `includes`-ing anything else need helpers we don't have → skip.
const SUPPORTED_INCLUDES: &[&str] = &["assert.js", "sta.js"];

/// The Test262 host-only specifier convention documented in
/// `test262/INTERPRETING.md`: implementers resolve `<module source>` to a
/// module that provides a valid Module Source (e.g. a WebAssembly module).
/// JustScript exposes the host `%AbstractModuleSource%` object as every
/// Module Record's `module_source_cell`, so loading this sentinel as an empty
/// JavaScript module is sufficient — the source-phase import binds to that
/// cell. The sentinel is intentionally NOT handled by the generic
/// `FileModuleLoader`; it lives here in the Test262 host wrapper.
const MODULE_SOURCE_SENTINEL: &str = "<module source>";

/// A Test262-specific module host: delegates ordinary specifiers to the
/// filesystem loader and resolves the `<module source>` virtual specifier to
/// one stable empty module. The stable identity is what lets two fixtures
/// importing `<module source>` share the same Module Record (and therefore
/// the same `module_source_cell`), which the source-phase ambiguity tests
/// rely on.
struct Test262ModuleLoader {
    fs: js_engine::FileModuleLoader,
}

impl Test262ModuleLoader {
    fn new() -> Self {
        Self {
            fs: js_engine::FileModuleLoader,
        }
    }
}

impl js_engine::ModuleLoader for Test262ModuleLoader {
    fn resolve(&self, referrer: Option<&str>, specifier: &str) -> Result<String, String> {
        if specifier == MODULE_SOURCE_SENTINEL {
            // Canonical key is the sentinel itself — every importer observes
            // the same identity, so the graph cache deduplicates the request.
            return Ok(MODULE_SOURCE_SENTINEL.to_string());
        }
        self.fs.resolve(referrer, specifier)
    }

    fn load(&self, key: &str) -> Result<std::sync::Arc<str>, String> {
        if key == MODULE_SOURCE_SENTINEL {
            // An empty JavaScript module. Its `module_source_cell` (populated
            // by the engine for every Module Record) is what source-phase
            // imports bind to; the body is never observed.
            return Ok(std::sync::Arc::<str>::from(""));
        }
        self.fs.load(key)
    }
}

/// Walk `root`, execute every runnable test in a **dedicated child process**
/// (so a single test's crash — stack overflow, OOM, hang — can't take the whole
/// runner down), and classify each outcome. Tests that are pure front-end cases
/// (parse/early negatives), async tests, or tests needing unsupported harness
/// helpers are skipped up front (no child spawned). Module tests execute with
/// a filesystem-backed host rooted at the test file's directory.
pub fn run_runtime(root: &Path) -> (Vec<RuntimeResult>, RuntimeStats) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("js-test262"));
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut results = Vec::new();
    let mut stats = RuntimeStats::default();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_js(&root, &mut files);
    files.sort();
    stats.files = files.len();

    for path in files {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                stats.skip += 1;
                stats.total += 1;
                results.push(RuntimeResult {
                    path,
                    variant: Variant::Sloppy,
                    features: Vec::new(),
                    expect: None,
                    outcome: RuntimeOutcome::Skip("unreadable".into()),
                });
                continue;
            }
        };
        let fm = match FrontMatter::parse_result(&src) {
            Ok(fm) => fm.unwrap_or_default(),
            Err(reason) => {
                stats.total += 1;
                stats.incomplete += 1;
                results.push(RuntimeResult {
                    path,
                    variant: Variant::Sloppy,
                    features: Vec::new(),
                    expect: None,
                    outcome: RuntimeOutcome::Incomplete(format!("metadata: {reason}")),
                });
                continue;
            }
        };
        let (expect, skip_reason) = classify_runtime(&Some(fm.clone()));
        for variant in variants(&fm) {
            stats.total += 1;
            let outcome = match &skip_reason {
                Some(reason) => RuntimeOutcome::Skip(reason.clone()),
                None => {
                    stats.executed += 1;
                    spawn_execute_one(&exe, &root, &path, variant)
                }
            };
            match &outcome {
                RuntimeOutcome::Pass => stats.pass += 1,
                RuntimeOutcome::Fail(_) => stats.fail += 1,
                RuntimeOutcome::Incomplete(_) => stats.incomplete += 1,
                RuntimeOutcome::Skip(_) => stats.skip += 1,
            }
            results.push(RuntimeResult {
                path: path.clone(),
                variant,
                features: fm.features.clone(),
                expect: expect.clone(),
                outcome,
            });
        }
    }

    (results, stats)
}

/// Spawn `<exe> execute-one <root> <relpath>` with a per-test timeout, and parse
/// its single-line outcome. A crash, timeout, or malformed reply is an
/// `Incomplete` — never a panic in the parent.
fn spawn_execute_one(exe: &Path, root: &Path, path: &Path, variant: Variant) -> RuntimeOutcome {
    let relpath = match path.strip_prefix(root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => path.to_path_buf(),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("execute-one")
        .arg(root)
        .arg(&relpath)
        .arg(variant.as_str())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return RuntimeOutcome::Incomplete(format!("could not spawn child: {e}")),
    };
    // Wait with a per-test timeout (test262 programs should run in well under a
    // second; a hung child is killed).
    const TIMEOUT_SECS: u64 = 10;
    let outcome = wait_with_timeout(child, TIMEOUT_SECS);
    parse_child_reply(&outcome)
}

/// Wait for a child process, killing it after `secs`. Returns its stdout (if it
/// exited normally) or a crash/timeout marker.
fn wait_with_timeout(mut child: std::process::Child, secs: u64) -> ChildExit {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().ok();
                let stdout = out
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if status.success() {
                    return ChildExit::Output(stdout);
                }
                return ChildExit::Crashed(stdout);
            }
            Ok(None) => {
                if std::time::Instant::now() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return ChildExit::Timeout;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(_) => return ChildExit::Crashed(String::new()),
        }
    }
}

enum ChildExit {
    Output(String),
    Crashed(String),
    Timeout,
}

/// Parse the single-line reply emitted by `execute-one`:
///   `PASS` | `FAIL\t<reason>` | `INCOMPLETE\t<reason>` | `SKIP\t<reason>`
fn parse_child_reply(exit: &ChildExit) -> RuntimeOutcome {
    let line = match exit {
        ChildExit::Output(s) => s.lines().next().unwrap_or("").trim().to_string(),
        ChildExit::Crashed(s) => {
            let reason = s.lines().next().unwrap_or("child process crashed");
            return RuntimeOutcome::Incomplete(format!("crashed: {reason}"));
        }
        ChildExit::Timeout => return RuntimeOutcome::Incomplete("timed out".into()),
    };
    let (tag, reason) = match line.split_once('\t') {
        Some((t, r)) => (t, r.to_string()),
        None => (line.as_str(), String::new()),
    };
    match tag {
        "PASS" => RuntimeOutcome::Pass,
        "FAIL" => RuntimeOutcome::Fail(reason),
        "INCOMPLETE" => RuntimeOutcome::Incomplete(if reason.is_empty() {
            "incomplete".into()
        } else {
            reason
        }),
        "SKIP" => RuntimeOutcome::Skip(reason),
        other => RuntimeOutcome::Incomplete(format!("bad reply: {other}")),
    }
}

/// Classify a test for the runtime phase. Returns the runtime expectation plus
/// an optional skip reason (when the test isn't a runtime test or needs support
/// we don't have).
pub fn classify_runtime(fm: &Option<FrontMatter>) -> (Option<RuntimeExpect>, Option<String>) {
    let f = match fm {
        Some(f) => f,
        None => return (Some(RuntimeExpect::CleanRun), None),
    };
    // Front-end-only negative phases: nothing to execute.
    match &f.negative_phase {
        Some(NegativePhase::Parse) => {
            return (None, Some("parse-phase negative (front-end)".into()))
        }
        Some(NegativePhase::Early) => {
            return (None, Some("early-error negative (not implemented)".into()))
        }
        Some(NegativePhase::Resolution) => {}
        _ => {}
    }
    // Flags we can't honour.
    for flag in &f.flags {
        match flag.as_str() {
            "module" => {}
            "async" => {}
            "raw" | "onlyStrict" | "noStrict" => {}
            _ => {}
        }
    }
    // Harness helper includes: we provide assert/sta natively; anything else we lack.
    for inc in &f.includes {
        if !SUPPORTED_INCLUDES.contains(&inc.as_str()) {
            return (None, Some(format!("unsupported include: {inc}")));
        }
    }
    let expect = match &f.negative_phase {
        Some(NegativePhase::Resolution) => RuntimeExpect::ResolutionThrows(
            f.negative_type
                .clone()
                .unwrap_or_else(|| "SyntaxError".into()),
        ),
        Some(NegativePhase::Runtime) => match &f.negative_type {
            Some(t) => RuntimeExpect::Throws(t.clone()),
            None => RuntimeExpect::CleanRun,
        },
        _ => RuntimeExpect::CleanRun,
    };
    (Some(expect), None)
}

/// Execute one test's source under a fresh engine with the test262 harness
/// installed (in this process). A panic is caught and surfaced as `Incomplete`.
/// Stack-overflow / OOM aborts cannot be caught here — the runtime runner calls
/// this inside a dedicated child process so such crashes are isolated.
pub fn execute_test_source(
    src: &str,
    expect: &Option<RuntimeExpect>,
    variant: Variant,
) -> RuntimeOutcome {
    let source = variant.source(src);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut engine = js_engine::Engine::default_interpreter();
        if variant != Variant::Raw {
            engine.install_test262_harness();
        }
        engine.execute(&source)
    }));
    match outcome {
        Ok(exec) => classify_outcome(exec, expect),
        Err(_) => RuntimeOutcome::Incomplete("engine PANICKED".into()),
    }
}

/// Execute a Test262 test with its host-visible file identity. Module tests use
/// this path as the entry module so relative `_FIXTURE.js` requests resolve in
/// exactly the same directory as the test.
pub fn execute_test_file(
    path: &Path,
    src: &str,
    expect: &Option<RuntimeExpect>,
    variant: Variant,
    async_test: bool,
) -> RuntimeOutcome {
    if variant != Variant::Module {
        let source = variant.source(src);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut engine = js_engine::Engine::default_interpreter();
            if variant != Variant::Raw {
                engine.install_test262_harness();
            }
            let execution = engine.execute(&source);
            (
                execution,
                engine.test262_done_called(),
                engine.test262_done_error(),
            )
        }));
        return match outcome {
            Ok((exec, done, done_error)) => {
                classify_async_outcome(exec, expect, async_test, done, done_error)
            }
            Err(_) => RuntimeOutcome::Incomplete("engine PANICKED".into()),
        };
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut engine = js_engine::Engine::default_interpreter();
        engine.install_test262_harness();
        let loader = Test262ModuleLoader::new();
        let execution = match engine.run_module(&path.display().to_string(), &loader) {
            Ok(result) => js_engine::ExecutionOutcome::Completed(result.value),
            Err(error) => js_engine::ExecutionOutcome::Failed(error),
        };
        (
            execution,
            engine.test262_done_called(),
            engine.test262_done_error(),
        )
    }));
    match outcome {
        Ok((exec, done, done_error)) => {
            classify_async_outcome(exec, expect, async_test, done, done_error)
        }
        Err(_) => RuntimeOutcome::Incomplete("engine PANICKED".into()),
    }
}

fn classify_async_outcome(
    exec: js_engine::ExecOutcome,
    expect: &Option<RuntimeExpect>,
    async_test: bool,
    done: bool,
    done_error: Option<js_engine::Value>,
) -> RuntimeOutcome {
    // `$DONE(value)` signals an async failure. The throw is swallowed by the
    // surrounding Promise reaction, so the recorded value is the only reliable
    // signal — it must force a FAIL even when the top-level script completed.
    if let Some(error) = done_error {
        let name = error.error_name().unwrap_or_else(|| "Error".into());
        return RuntimeOutcome::Fail(format!("async $DONE({name})"));
    }
    let outcome = classify_outcome(exec, expect);
    if async_test && matches!(outcome, RuntimeOutcome::Pass) && !done {
        RuntimeOutcome::Fail("async test completed without calling $DONE".into())
    } else {
        outcome
    }
}

fn classify_outcome(
    exec: js_engine::ExecOutcome,
    expect: &Option<RuntimeExpect>,
) -> RuntimeOutcome {
    use js_engine::{EngineError, ExecutionOutcome};
    match expect {
        Some(RuntimeExpect::CleanRun) => match exec {
            ExecutionOutcome::Completed(_) => RuntimeOutcome::Pass,
            ExecutionOutcome::Failed(EngineError::Exception(error)) => {
                let name = error.value.error_name().unwrap_or_else(|| "Error".into());
                RuntimeOutcome::Fail(format!("threw {name}"))
            }
            ExecutionOutcome::Failed(EngineError::Compile(report)) => {
                RuntimeOutcome::Incomplete(format!(
                    "compile error: {}",
                    report
                        .first()
                        .map(|d| d.message.clone())
                        .unwrap_or_default()
                ))
            }
            ExecutionOutcome::Failed(EngineError::Fault(error)) => {
                RuntimeOutcome::Incomplete(format!("vm: {}", error.message))
            }
            ExecutionOutcome::Failed(EngineError::Module(error)) => {
                RuntimeOutcome::Incomplete(format!("module: {error}"))
            }
        },
        Some(RuntimeExpect::Throws(want)) => match exec {
            ExecutionOutcome::Failed(EngineError::Exception(error)) => {
                let got = error.value.error_name().unwrap_or_else(|| "Error".into());
                if &got == want {
                    RuntimeOutcome::Pass
                } else {
                    RuntimeOutcome::Fail(format!("expected {want}, threw {got}"))
                }
            }
            ExecutionOutcome::Completed(_) => {
                RuntimeOutcome::Fail(format!("expected {want}, nothing thrown"))
            }
            ExecutionOutcome::Failed(EngineError::Compile(report)) => {
                RuntimeOutcome::Incomplete(format!(
                    "compile error: {}",
                    report
                        .first()
                        .map(|d| d.message.clone())
                        .unwrap_or_default()
                ))
            }
            ExecutionOutcome::Failed(EngineError::Fault(error)) => {
                RuntimeOutcome::Incomplete(format!("vm: {}", error.message))
            }
            ExecutionOutcome::Failed(EngineError::Module(error)) => {
                RuntimeOutcome::Incomplete(format!("module: {error}"))
            }
        },
        Some(RuntimeExpect::ResolutionThrows(want)) => match exec {
            ExecutionOutcome::Failed(EngineError::Module(_))
            | ExecutionOutcome::Failed(EngineError::Compile(_))
                if want == "SyntaxError" =>
            {
                RuntimeOutcome::Pass
            }
            ExecutionOutcome::Failed(EngineError::Exception(error)) => {
                let got = error.value.error_name().unwrap_or_else(|| "Error".into());
                RuntimeOutcome::Fail(format!("expected resolution {want}, threw runtime {got}"))
            }
            ExecutionOutcome::Completed(_) => {
                RuntimeOutcome::Fail(format!("expected resolution {want}, nothing thrown"))
            }
            ExecutionOutcome::Failed(EngineError::Module(error)) => RuntimeOutcome::Fail(format!(
                "expected resolution {want}, got module error: {error}"
            )),
            ExecutionOutcome::Failed(EngineError::Compile(_)) => RuntimeOutcome::Fail(format!(
                "expected resolution {want}, got compile SyntaxError"
            )),
            ExecutionOutcome::Failed(EngineError::Fault(error)) => {
                RuntimeOutcome::Incomplete(format!("vm: {}", error.message))
            }
        },
        None => RuntimeOutcome::Skip("not a runtime test".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use js_engine::ModuleLoader;

    fn fm(flags: &[&str]) -> FrontMatter {
        FrontMatter {
            flags: flags.iter().map(|f| f.to_string()).collect(),
            ..FrontMatter::default()
        }
    }

    #[test]
    fn expands_test262_variants() {
        assert_eq!(variants(&fm(&[])), [Variant::Sloppy, Variant::Strict]);
        assert_eq!(variants(&fm(&["onlyStrict"])), [Variant::Strict]);
        assert_eq!(variants(&fm(&["noStrict"])), [Variant::Sloppy]);
        assert_eq!(variants(&fm(&["module"])), [Variant::Module]);
        assert_eq!(variants(&fm(&["raw"])), [Variant::Raw]);
    }

    #[test]
    fn early_phase_is_a_parse_error_expectation() {
        let metadata = FrontMatter {
            negative_phase: Some(NegativePhase::Early),
            ..FrontMatter::default()
        };
        assert_eq!(classify(&metadata), Expect::Err);
    }

    #[test]
    fn fixture_files_are_not_collected() {
        let dir = std::env::temp_dir().join(format!("justscript-test262-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("case.js"), "").unwrap();
        std::fs::write(dir.join("dep_FIXTURE.js"), "").unwrap();

        let mut files = Vec::new();
        collect_js(&dir, &mut files);
        assert_eq!(files, [dir.join("case.js")]);

        std::fs::remove_dir_all(dir).unwrap();
    }

    /// The Test262 host wrapper resolves the virtual `<module source>`
    /// sentinel to a single stable canonical key, regardless of referrer, so
    /// every importer observes the same Module Record (C4). Ordinary
    /// specifiers delegate to the filesystem loader.
    #[test]
    fn test262_loader_resolves_module_source_sentinel_stably() {
        let loader = Test262ModuleLoader::new();
        let from_a = loader
            .resolve(Some("/dir/a.js"), "<module source>")
            .unwrap();
        let from_b = loader
            .resolve(Some("/other/b.js"), "<module source>")
            .unwrap();
        assert_eq!(from_a, "<module source>");
        assert_eq!(from_a, from_b, "sentinel must canonicalize to one identity");
        // Loads to an empty module body (the ModuleSource cell is what matters;
        // it is populated by the engine for every record).
        assert_eq!(loader.load("<module source>").unwrap().as_ref(), "");
    }

    /// The Test262 host wrapper still delegates ordinary specifiers to the
    /// filesystem loader (the sentinel is the ONLY virtual path).
    #[test]
    fn test262_loader_delegates_ordinary_specifiers_to_filesystem() {
        let dir = std::env::temp_dir().join(format!("justscript-t262-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("real.js");
        std::fs::write(&path, "x").unwrap();
        let loader = Test262ModuleLoader::new();
        let resolved = loader
            .resolve(Some(&path.display().to_string()), "./real.js")
            .unwrap();
        assert_eq!(loader.load(&resolved).unwrap().as_ref(), "x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Regression: `$DONE(value)` signals an async failure. The throw happens
    /// inside a Promise reaction and used to be swallowed, so the realm only
    /// recorded `done_called = true` and failing async tests were classified
    /// as PASS. The realm now retains the failure value and the runner MUST
    /// classify the test as `Fail`.
    #[test]
    fn done_called_with_an_error_is_classified_as_failure() {
        let src = "Promise.resolve().then(function () {\n\
                   \x20\x20$DONE(new Test262Error(\"forced async failure\"));\n\
                   });";
        let path = std::path::Path::new("done-error.js");
        let expect = Some(RuntimeExpect::CleanRun);
        let outcome = execute_test_file(path, src, &expect, Variant::Sloppy, true);
        match outcome {
            RuntimeOutcome::Fail(msg) => assert!(
                msg.contains("$DONE"),
                "expected an async-$DONE failure, got: {msg}"
            ),
            other => panic!("expected Fail, got {other:?}"),
        }
    }

    /// Guard against over-triggering: a successful `$DONE()` (no argument) in a
    /// Promise reaction must still PASS.
    #[test]
    fn done_called_with_no_argument_still_passes() {
        let src = "Promise.resolve().then(function () { $DONE(); });";
        let path = std::path::Path::new("done-ok.js");
        let expect = Some(RuntimeExpect::CleanRun);
        let outcome = execute_test_file(path, src, &expect, Variant::Sloppy, true);
        assert!(
            matches!(outcome, RuntimeOutcome::Pass),
            "expected Pass, got {outcome:?}"
        );
    }
}
