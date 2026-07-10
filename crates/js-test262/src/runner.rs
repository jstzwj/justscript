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
