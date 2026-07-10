//! `js-test262` — parse-phase conformance runner CLI.
//!
//! Usage:
//!   js-test262 run <test262-root> [--dir test/language/asi] [--show-fails N]
//!   js-test262 false-accepts <test262-root>   # list only false-accept bugs

use js_test262::{run, Outcome, TestResult};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "usage:\n  {} run <test262-root> [--dir <sub>] [--show-fails <N>] [--list-false-accepts] [--cluster]\n  {} false-accepts <test262-root>",
            args[0], args[0]
        );
        return ExitCode::from(2);
    }
    let _cmd = &args[1];
    let root = PathBuf::from(&args[2]);

    let mut subdir: Option<String> = None;
    let mut show_fails: usize = 0;
    let mut only_false_accepts = false;
    let mut cluster = false;
    let mut rejects_with: Option<String> = None;
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
        "scanned: {} | pass: {} | fail: {} | skip: {}",
        stats.total, stats.pass, stats.fail, stats.skip
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
        let judged = stats.pass + stats.fail;
        if judged == 0 {
            0.0
        } else {
            100.0 * stats.pass as f64 / judged as f64
        }
    });

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
                println!("  {}", rel(&r.path, &root));
            }
        }
        return ExitCode::SUCCESS;
    }

    if show_fails > 0 {
        println!("\n=== first {} false-rejects (sample) ===", show_fails);
        let mut shown = 0;
        for r in &fails {
            if r.expect == js_test262::Expect::Ok && shown < show_fails {
                println!("  {}", rel(&r.path, &root));
                shown += 1;
            }
        }
    }

    ExitCode::SUCCESS
}

fn rel(path: &std::path::Path, base: &std::path::Path) -> String {
    path.strip_prefix(base).map(|p| p.display().to_string()).unwrap_or_else(|_| path.display().to_string())
}

/// Cluster false-rejects (valid syntax we reject) by feature directory and by
/// normalized parser message, so the largest grammar gaps surface first.
fn cluster_fails(results: &[TestResult], root: &std::path::Path) {
    use std::collections::BTreeMap;

    // (count, sample paths) per bucket key.
    let mut by_dir: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut by_msg: BTreeMap<String, (usize, Vec<String>)> = BTreeMap::new();
    let mut total = 0usize;

    for r in results {
        if r.expect != js_test262::Expect::Ok {
            continue;
        }
        let Outcome::Fail { reason } = &r.outcome else { continue };
        total += 1;
        let relp = rel(&r.path, root);

        let feat = feature_dir(&relp);
        let e = by_dir.entry(feat).or_insert((0, Vec::new()));
        e.0 += 1;
        if e.1.len() < 3 { e.1.push(relp.clone()); }

        let nm = normalize_msg(reason);
        let e = by_msg.entry(nm).or_insert((0, Vec::new()));
        e.0 += 1;
        if e.1.len() < 3 { e.1.push(relp); }
    }

    println!("\n=== false-rejects clustered by FEATURE DIRECTORY ({} total) ===", total);
    let mut dirs: Vec<_> = by_dir.into_iter().collect();
    dirs.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (feat, (n, samples)) in dirs.iter().take(40) {
        println!("  {:5}  {}", n, feat);
        for s in samples { println!("           e.g. {}", s); }
    }

    println!("\n=== false-rejects clustered by PARSER MESSAGE (top 30) ===");
    let mut msgs: Vec<_> = by_msg.into_iter().collect();
    msgs.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    for (msg, (n, samples)) in msgs.iter().take(30) {
        println!("  {:5}  {}", n, msg);
        for s in samples { println!("           e.g. {}", s); }
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
        take.iter().map(|s| s.to_string()).collect::<Vec<_>>().join("/")
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
