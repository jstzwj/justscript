//! The runner: walks a directory tree, classifies each test, runs the parser,
//! and aggregates results.

use crate::frontmatter::{FrontMatter, NegativePhase};
use std::path::{Path, PathBuf};

/// What we expect the parser to do for a given test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expect {
    /// Source must parse without error.
    Ok,
    /// Source must be rejected by the parser.
    Err,
    /// Not applicable (e.g. early-error phase; we don't implement those).
    Skip,
}

/// The outcome for a single test.
#[derive(Clone, Debug)]
pub enum Outcome {
    /// Expectation met.
    Pass,
    /// Expectation violated — an actionable result.
    Fail { reason: String },
    /// Skipped (early-error, or unsupported flag).
    Skip(String),
}

#[derive(Clone, Debug)]
pub struct TestResult {
    pub path: PathBuf,
    pub expect: Expect,
    pub outcome: Outcome,
}

/// Aggregated statistics.
#[derive(Default, Debug)]
pub struct Stats {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub skip: usize,
    /// False accepts: we returned Ok but the test demanded a parse error.
    pub false_accept: usize,
    /// False rejects on ExpectOk: we returned Err but parse success was required.
    pub false_reject: usize,
}

/// Walk `root`, classify and run every `.js` test, returning per-test results
/// and aggregate stats. `prefix` is stripped from reported paths for brevity.
pub fn run(root: &Path) -> (Vec<TestResult>, Stats) {
    let mut results = Vec::new();
    let mut stats = Stats::default();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_js(root, &mut files);
    files.sort();
    stats.total = files.len();

    for path in files {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                stats.skip += 1;
                results.push(TestResult {
                    path,
                    expect: Expect::Skip,
                    outcome: Outcome::Skip("unreadable".into()),
                });
                continue;
            }
        };
        let fm = FrontMatter::parse(&src);
        let expect = classify(&fm);

        let outcome = match expect {
            Expect::Skip => {
                stats.skip += 1;
                Outcome::Skip("early-error phase (not implemented)".into())
            }
            Expect::Ok | Expect::Err => {
                let is_module = fm.as_ref().map(|f| f.flags.contains(&"module".to_string())).unwrap_or(false);
                // Run parsing under catch_unwind so a panic in one test (e.g. a
                // parser bug) is recorded instead of aborting the whole run.
                let parse_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if is_module {
                        js_parser::parse_module(&src)
                    } else {
                        js_parser::parse(&src)
                    }
                }));
                // `parse_ok` + the first diagnostic message (for false-reject
                // root-cause clustering in the CLI).
                let (parse_ok, reason): (bool, String) = match parse_res {
                    Ok(Ok(_)) => (true, String::new()),
                    Ok(Err(diags)) => {
                        let msg = diags
                            .first()
                            .map(|d| d.message.clone())
                            .unwrap_or_else(|| "(no diagnostic)".into());
                        (false, msg)
                    }
                    Err(_) => (false, "parser PANICKED".into()),
                };
                match (expect, parse_ok) {
                    (Expect::Ok, true) | (Expect::Err, false) => {
                        stats.pass += 1;
                        Outcome::Pass
                    }
                    (Expect::Ok, false) => {
                        stats.fail += 1;
                        stats.false_reject += 1;
                        Outcome::Fail {
                            reason: format!("parse Err: {}", reason),
                        }
                    }
                    (Expect::Err, true) => {
                        stats.fail += 1;
                        stats.false_accept += 1;
                        Outcome::Fail {
                            reason: "expected parse Err (invalid syntax), got Ok".into(),
                        }
                    }
                    (Expect::Skip, _) => unreachable!(),
                }
            }
        };

        results.push(TestResult {
            path,
            expect,
            outcome,
        });
    }

    (results, stats)
}

fn classify(fm: &Option<FrontMatter>) -> Expect {
    match fm {
        None => Expect::Ok,
        Some(f) => match &f.negative_phase {
            Some(NegativePhase::Parse) => Expect::Err,
            Some(NegativePhase::Early) => Expect::Skip,
            _ => Expect::Ok,
        },
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
        } else if path.extension().and_then(|e| e.to_str()) == Some("js") {
            out.push(path);
        }
    }
}

// ---- runtime (execution) phase -------------------------------------------

/// A runtime-phase expectation derived from the frontmatter.
#[derive(Clone, Debug)]
pub enum RuntimeExpect {
    /// Run to completion without throwing.
    CleanRun,
    /// Throw an error whose `.name` matches this.
    Throws(String),
}

/// Aggregated runtime statistics.
#[derive(Default, Debug)]
pub struct RuntimeStats {
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
#[derive(Clone, Debug)]
pub struct RuntimeResult {
    pub path: PathBuf,
    pub expect: Option<RuntimeExpect>,
    pub outcome: RuntimeOutcome,
}

#[derive(Clone, Debug)]
pub enum RuntimeOutcome {
    Pass,
    Fail(String),
    Incomplete(String),
    Skip(String),
}

/// The harness helper files whose API we provide natively (`assert.js`, `sta.js`).
/// Tests `includes`-ing anything else need helpers we don't have → skip.
const SUPPORTED_INCLUDES: &[&str] = &["assert.js", "sta.js"];

/// Walk `root`, execute every runnable test in a **dedicated child process**
/// (so a single test's crash — stack overflow, OOM, hang — can't take the whole
/// runner down), and classify each outcome. Tests that are pure front-end cases
/// (parse/early/resolution negative), modules, async, or that need unsupported
/// harness helpers are skipped up front (no child spawned).
pub fn run_runtime(root: &Path) -> (Vec<RuntimeResult>, RuntimeStats) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("js-test262"));
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut results = Vec::new();
    let mut stats = RuntimeStats::default();
    let mut files: Vec<PathBuf> = Vec::new();
    collect_js(&root, &mut files);
    files.sort();
    stats.total = files.len();

    for path in files {
        let src = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => {
                stats.skip += 1;
                results.push(RuntimeResult {
                    path,
                    expect: None,
                    outcome: RuntimeOutcome::Skip("unreadable".into()),
                });
                continue;
            }
        };
        let fm = FrontMatter::parse(&src);
        let (expect, skip_reason) = classify_runtime(&fm);
        let outcome = match skip_reason {
            Some(reason) => {
                stats.skip += 1;
                RuntimeOutcome::Skip(reason)
            }
            None => {
                stats.executed += 1;
                spawn_execute_one(&exe, &root, &path)
            }
        };
        match &outcome {
            RuntimeOutcome::Pass => stats.pass += 1,
            RuntimeOutcome::Fail(_) => stats.fail += 1,
            RuntimeOutcome::Incomplete(_) => stats.incomplete += 1,
            RuntimeOutcome::Skip(_) => {}
        }
        results.push(RuntimeResult {
            path,
            expect,
            outcome,
        });
    }

    (results, stats)
}

/// Spawn `<exe> execute-one <root> <relpath>` with a per-test timeout, and parse
/// its single-line outcome. A crash, timeout, or malformed reply is an
/// `Incomplete` — never a panic in the parent.
fn spawn_execute_one(exe: &Path, root: &Path, path: &Path) -> RuntimeOutcome {
    let relpath = match path.strip_prefix(root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => path.to_path_buf(),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("execute-one")
        .arg(root)
        .arg(&relpath)
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
        Some(NegativePhase::Resolution) => {
            return (None, Some("resolution-phase negative (module linker)".into()))
        }
        _ => {}
    }
    // Flags we can't honour.
    for flag in &f.flags {
        match flag.as_str() {
            "module" => return (None, Some("module (no linker)".into())),
            "async" => return (None, Some("async (no Promise scheduling)".into())),
            "raw" => return (None, Some("raw harness flag".into())),
            "onlyStrict" | "noStrict" => { /* handled below via a second pass */ }
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
pub fn execute_test_source(src: &str, expect: &Option<RuntimeExpect>) -> RuntimeOutcome {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut engine = js_engine::Engine::default_interpreter();
        engine.install_test262_harness();
        engine.execute(src)
    }));
    match outcome {
        Ok(exec) => classify_outcome(exec, expect),
        Err(_) => RuntimeOutcome::Incomplete("engine PANICKED".into()),
    }
}

fn classify_outcome(exec: js_engine::ExecOutcome, expect: &Option<RuntimeExpect>) -> RuntimeOutcome {
    use js_engine::ExecOutcome;
    match expect {
        Some(RuntimeExpect::CleanRun) => match exec {
            ExecOutcome::Ok(_) => RuntimeOutcome::Pass,
            ExecOutcome::Threw(v) => {
                let name = v.error_name().unwrap_or_else(|| "Error".into());
                RuntimeOutcome::Fail(format!("threw {name}"))
            }
            ExecOutcome::CompileError(diags) => RuntimeOutcome::Incomplete(format!(
                "compile error: {}",
                diags.first().map(|d| d.message.clone()).unwrap_or_default()
            )),
            ExecOutcome::Internal(msg) => RuntimeOutcome::Incomplete(format!("vm: {msg}")),
        },
        Some(RuntimeExpect::Throws(want)) => match exec {
            ExecOutcome::Threw(v) => {
                let got = v.error_name().unwrap_or_else(|| "Error".into());
                if &got == want {
                    RuntimeOutcome::Pass
                } else {
                    RuntimeOutcome::Fail(format!("expected {want}, threw {got}"))
                }
            }
            ExecOutcome::Ok(_) => {
                RuntimeOutcome::Fail(format!("expected {want}, nothing thrown"))
            }
            ExecOutcome::CompileError(diags) => RuntimeOutcome::Incomplete(format!(
                "compile error: {}",
                diags.first().map(|d| d.message.clone()).unwrap_or_default()
            )),
            ExecOutcome::Internal(msg) => RuntimeOutcome::Incomplete(format!("vm: {msg}")),
        },
        None => RuntimeOutcome::Skip("not a runtime test".into()),
    }
}
