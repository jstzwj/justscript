//! `js-test262` — parse-phase conformance runner CLI.
//!
//! Usage:
//!   js-test262 run <test262-root> [--dir test/language/asi] [--show-fails N]
//!   js-test262 false-accepts <test262-root>   # list only false-accept bugs

use js_test262::{run, run_runtime, Outcome, TestResult, Variant};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage:\n  {} run <test262-root> [--dir <sub>] [--show-fails <N>] [--list-false-accepts] [--cluster] [--json <path>]\n  {} execute <test262-root> [--dir <sub>] [--show-fails <N>] [--cluster] [--json <path>]\n  {} false-accepts <test262-root>",
            args[0], args[0], args[0]
        );
        return ExitCode::from(2);
    }
    let cmd = &args[1];
    let root = PathBuf::from(&args[2]);

    // `execute` is the runtime phase — dispatch to a separate path.
    if cmd == "execute" {
        return run_execute_mode(&args, &root);
    }

    // `execute-one` runs a single test (emitting one outcome line). Invoked by
    // the parent `execute` mode as an isolated child process per test.
    if cmd == "execute-one" {
        return run_execute_one(&args);
    }
    if cmd != "run" && cmd != "false-accepts" {
        eprintln!("error: unknown command `{cmd}`");
        return ExitCode::from(2);
    }

    let mut subdir: Option<String> = None;
    let mut show_fails: usize = 0;
    let mut only_false_accepts = cmd == "false-accepts";
    let mut cluster = false;
    let mut rejects_with: Option<String> = None;
    let mut json_path: Option<String> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                subdir = args.get(i + 1).cloned();
                i += 2;
            }
            "--show-fails" => {
                show_fails = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
                i += 2;
            }
            "--list-false-accepts" => {
                only_false_accepts = true;
                i += 1;
            }
            "--cluster" => {
                cluster = true;
                i += 1;
            }
            "--rejects-with" => {
                rejects_with = args.get(i + 1).cloned();
                i += 2;
            }
            "--json" => {
                json_path = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let scan_root = match &subdir {
        Some(s) => root.join(s),
        None => root.join("test/language"),
    };

    if !scan_root.exists() {
        eprintln!("error: scan root does not exist: {}", scan_root.display());
        return ExitCode::from(2);
    }

    let (results, stats) = run(&scan_root);

    let fails: Vec<&TestResult> = results
        .iter()
        .filter(|r| matches!(r.outcome, Outcome::Fail { .. }))
        .collect();

    println!(
        "files: {} | variants: {} | pass: {} | fail: {} | incomplete: {} | skip: {}",
        stats.files, stats.total, stats.pass, stats.fail, stats.incomplete, stats.skip
    );
    println!(
        "  false-accept (accepted invalid syntax): {}",
        stats.false_accept
    );
    println!(
        "  false-reject (rejected valid syntax):   {}",
        stats.false_reject
    );
    println!("pass rate over non-skipped: {:.1}%", {
        let judged = stats.pass + stats.fail + stats.incomplete;
        if judged == 0 {
            0.0
        } else {
            100.0 * stats.pass as f64 / judged as f64
        }
    });

    if let Some(path) = json_path {
        let report = serde_json::json!({
            "schema": 1,
            "mode": "parse",
            "test262_revision": include_str!("../../../test262-revision.txt").trim(),
            "stats": &stats,
            "results": &results,
        });
        if let Err(e) = write_json(&path, &report) {
            eprintln!("error: cannot write JSON report: {e}");
            return ExitCode::from(2);
        }
    }

    if cluster {
        cluster_fails(&results, &root);
        return ExitCode::SUCCESS;
    }

    if let Some(needle) = &rejects_with {
        for r in &results {
            if r.expect == js_test262::Expect::Ok {
                if let Outcome::Fail { reason } = &r.outcome {
                    if reason.contains(needle) {
                        println!("{}", rel(&r.path, &root));
                    }
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    if only_false_accepts {
        println!("\n=== false-accept bugs (we accept code that must be rejected) ===");
        for r in &fails {
            if r.expect == js_test262::Expect::Err {
                println!("  {} [{}]", rel(&r.path, &root), r.variant.as_str());
            }
        }
        return ExitCode::SUCCESS;
    }

    if show_fails > 0 {
        println!("\n=== first {} false-rejects (sample) ===", show_fails);
        let mut shown = 0;
        for r in &fails {
            if r.expect == js_test262::Expect::Ok && shown < show_fails {
                println!("  {} [{}]", rel(&r.path, &root), r.variant.as_str());
                shown += 1;
            }
        }
    }

    ExitCode::SUCCESS
}

fn rel(path: &std::path::Path, base: &std::path::Path) -> String {
    path.strip_prefix(base)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn write_json(path: &str, report: &serde_json::Value) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(report).expect("JSON report must serialize");
    std::fs::write(path, bytes)
}

/// `execute <root>` — the runtime (execution) conformance phase. Runs each test
/// under a fresh realm with the test262 harness installed and classifies the
/// outcome (pass / fail / incomplete / skip).
fn run_execute_mode(args: &[String], root: &std::path::Path) -> ExitCode {
    let mut subdir: Option<String> = None;
    let mut show_fails: usize = 0;
    let mut cluster = false;
    let mut json_path: Option<String> = None;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" => {
                subdir = args.get(i + 1).cloned();
                i += 2;
            }
            "--show-fails" => {
                show_fails = args.get(i + 1).and_then(|s| s.parse().ok()).unwrap_or(0);
                i += 2;
            }
            "--cluster" => {
                cluster = true;
                i += 1;
            }
            "--json" => {
                json_path = args.get(i + 1).cloned();
                i += 2;
            }
            _ => i += 1,
        }
    }

    let scan_root = match &subdir {
        Some(s) => root.join(s),
        None => root.join("test/language"),
    };
    if !scan_root.exists() {
        eprintln!("error: scan root does not exist: {}", scan_root.display());
        return ExitCode::from(2);
    }

    let (results, stats) = run_runtime(&scan_root);

    println!(
        "files: {} | variants: {} | executed: {} | pass: {} | fail: {} | incomplete: {} | skip: {}",
        stats.files,
        stats.total,
        stats.executed,
        stats.pass,
        stats.fail,
        stats.incomplete,
        stats.skip
    );
    if stats.executed > 0 {
        println!(
            "  runtime pass rate over executed: {:.1}%",
            100.0 * stats.pass as f64 / stats.executed as f64
        );
    }

    if let Some(path) = json_path {
        let report = serde_json::json!({
            "schema": 1,
            "mode": "runtime",
            "test262_revision": include_str!("../../../test262-revision.txt").trim(),
            "stats": &stats,
            "results": &results,
        });
        if let Err(e) = write_json(&path, &report) {
            eprintln!("error: cannot write JSON report: {e}");
            return ExitCode::from(2);
        }
    }

    if cluster {
        cluster_runtime(&results, root);
        return ExitCode::SUCCESS;
    }

    if show_fails > 0 {
        println!("\n=== first {} runtime failures (sample) ===", show_fails);
        let mut shown = 0;
        for r in &results {
            if shown >= show_fails {
                break;
            }
            let reason = match &r.outcome {
                js_test262::RuntimeOutcome::Fail(reason) => Some(reason.as_str()),
                js_test262::RuntimeOutcome::Incomplete(reason) => Some(reason.as_str()),
                _ => None,
            };
            if let Some(reason) = reason {
                println!(
                    "  {} [{}] ({})",
                    rel(&r.path, root),
                    r.variant.as_str(),
                    reason
                );
                shown += 1;
            }
        }
        if shown == 0 {
            println!("  (no failures)");
        }
    }

    ExitCode::SUCCESS
}

/// Cluster runtime failures by feature directory + normalized reason, so the
/// biggest execution gaps surface first.
fn cluster_runtime(results: &[js_test262::RuntimeResult], root: &std::path::Path) {
    use std::collections::BTreeMap;
    let mut by_dir: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut by_reason: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut total = 0usize;
    for r in results {
        let (kind, reason) = match &r.outcome {
            js_test262::RuntimeOutcome::Fail(reason) => ("fail", reason.clone()),
            js_test262::RuntimeOutcome::Incomplete(reason) => ("incomplete", reason.clone()),
            _ => continue,
        };
        total += 1;
        let relp = rel(&r.path, root);
        let feat = feature_dir(&relp);
        let e = by_dir.entry(feat).or_default();
        e.0 += 1;
        if e.1.len() < 3 {
            e.1.push(relp.clone());
        }
        let nm = normalize_runtime_reason(&reason);
        let e = by_reason.entry(format!("{kind}: {nm}")).or_default();
        e.0 += 1;
        if e.1.len() < 3 {
            e.1.push(relp);
        }
    }
    println!(
        "\n=== runtime actionable outcomes by FEATURE DIRECTORY ({} total) ===",
        total
    );
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (feat, (n, samples)) in dirs.iter().take(40) {
        println!("  {:5}  {}", n, feat);
        for s in samples {
            println!("           e.g. {}", s);
        }
    }
    println!("\n=== runtime actionable outcomes by REASON (top 30) ===");
    let mut msgs: Vec<_> = by_reason.into_iter().collect();
    msgs.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (msg, (n, samples)) in msgs.iter().take(30) {
        println!("  {:5}  {}", n, msg);
        for s in samples {
            println!("           e.g. {}", s);
        }
    }
}

fn normalize_runtime_reason(reason: &str) -> String {
    // vm internal errors carry a trailing detail; bucket by the kind prefix.
    if let Some(idx) = reason.find(" (") {
        return reason[..idx].to_string();
    }
    reason.to_string()
}

/// `execute-one <root> <relpath>` — child-process entry: read one test, classify
/// it, run it in-process, and emit a single outcome line on stdout:
///   `PASS` | `FAIL\t<reason>` | `INCOMPLETE\t<reason>` | `SKIP\t<reason>`
fn run_execute_one(args: &[String]) -> ExitCode {
    if args.len() < 5 {
        println!("INCOMPLETE\tmissing execute-one variant");
        return ExitCode::SUCCESS;
    }
    let root = PathBuf::from(&args[2]);
    let relpath = PathBuf::from(&args[3]);
    let path = root.join(&relpath);
    let Some(variant) = Variant::parse(&args[4]) else {
        println!("INCOMPLETE\tinvalid execute-one variant");
        return ExitCode::SUCCESS;
    };
    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            println!("INCOMPLETE\tunreadable: {e}");
            return ExitCode::SUCCESS;
        }
    };
    let fm = match js_test262::FrontMatter::parse_result(&src) {
        Ok(fm) => fm,
        Err(e) => {
            println!("INCOMPLETE\tmetadata: {}", one_line(&e));
            return ExitCode::SUCCESS;
        }
    };
    let (expect, skip_reason) = js_test262::classify_runtime(&fm);
    if let Some(reason) = skip_reason {
        println!("SKIP\t{}", reason);
        return ExitCode::SUCCESS;
    }
    let async_test = fm
        .as_ref()
        .is_some_and(|metadata| metadata.flags.iter().any(|flag| flag == "async"));
    let outcome = js_test262::execute_test_file(&path, &src, &expect, variant, async_test);
    match outcome {
        js_test262::RuntimeOutcome::Pass => println!("PASS"),
        js_test262::RuntimeOutcome::Fail(r) => println!("FAIL\t{}", one_line(&r)),
        js_test262::RuntimeOutcome::Incomplete(r) => println!("INCOMPLETE\t{}", one_line(&r)),
        js_test262::RuntimeOutcome::Skip(r) => println!("SKIP\t{}", r),
    }
    ExitCode::SUCCESS
}

/// Collapse a reason to a single line (no embedded newlines/tabs).
fn one_line(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Cluster both false accepts and false rejects so correctness gaps remain
/// visible alongside unsupported valid syntax.
fn cluster_fails(results: &[TestResult], root: &std::path::Path) {
    use std::collections::BTreeMap;

    // (count, sample paths) per bucket key.
    let mut by_dir: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut by_msg: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut total = 0usize;

    for r in results {
        let Outcome::Fail { reason } = &r.outcome else {
            continue;
        };
        total += 1;
        let relp = rel(&r.path, root);
        let kind = match r.expect {
            js_test262::Expect::Err => "false-accept",
            js_test262::Expect::Ok => "false-reject",
            js_test262::Expect::Skip => "unexpected",
        };

        let feat = feature_dir(&relp);
        let e = by_dir
            .entry(format!("{kind}: {feat}"))
            .or_insert((0, Vec::new()));
        e.0 += 1;
        if e.1.len() < 3 {
            e.1.push(format!("{} [{}]", relp, r.variant.as_str()));
        }

        let nm = if kind == "false-accept" {
            "accepted invalid syntax".to_string()
        } else {
            normalize_msg(reason)
        };
        let e = by_msg
            .entry(format!("{kind}: {nm}"))
            .or_insert((0, Vec::new()));
        e.0 += 1;
        if e.1.len() < 3 {
            e.1.push(format!("{} [{}]", relp, r.variant.as_str()));
        }
    }

    println!(
        "\n=== parser failures by FEATURE DIRECTORY ({} total) ===",
        total
    );
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (feat, (n, samples)) in dirs.iter().take(40) {
        println!("  {:5}  {}", n, feat);
        for s in samples {
            println!("           e.g. {}", s);
        }
    }

    println!("\n=== parser failures by OUTCOME/REASON (top 30) ===");
    let mut msgs: Vec<_> = by_msg.into_iter().collect();
    msgs.sort_by(|a, b| b.1 .0.cmp(&a.1 .0));
    for (msg, (n, samples)) in msgs.iter().take(30) {
        println!("  {:5}  {}", n, msg);
        for s in samples {
            println!("           e.g. {}", s);
        }
    }
}

/// Reduce a test path to its feature area, e.g.
/// `test/language/statements/let/syntax/...` → `statements/let`.
fn feature_dir(rel: &str) -> String {
    let comps: Vec<&str> = rel.split('/').collect();
    // find the component after "language" (or "built-ins") and take the next two.
    let start = comps
        .iter()
        .position(|c| *c == "language" || *c == "built-ins" || *c == "annexB");
    let base = start.map(|i| i + 1).unwrap_or(0);
    let take: Vec<&&str> = comps[base.min(comps.len())..].iter().take(2).collect();
    if take.is_empty() {
        "(root)".into()
    } else {
        take.iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Collapse dynamic token substitutions in a parser message so templated
/// failures collapse into one bucket (e.g. "expected `;`, found LBrace" and
/// "expected `;`, found KwLet" both become "expected `;`, found <X>").
fn normalize_msg(reason: &str) -> String {
    // Drop the "parse Err: " prefix the runner adds.
    let m = reason.strip_prefix("parse Err: ").unwrap_or(reason).trim();
    // Cut at the first dynamic substitution: "found <rest>" or ": <rest>".
    if let Some(idx) = m.find("found ") {
        let mut s = String::from(&m[..idx + "found ".len()]);
        s.push_str("<X>");
        return s;
    }
    if let Some(idx) = m.find(": ") {
        let mut s = String::from(&m[..idx + ": ".len()]);
        s.push_str("<X>");
        return s;
    }
    m.to_string()
}
