//! test262 parse-phase conformance harness.
//!
//! Walks a directory of test262 tests, reads each test's frontmatter, and
//! checks that [`js_parser`] agrees with the expected parse outcome:
//! - `negative.phase: parse` → the source must fail to parse (ExpectErr),
//! - everything else (including `runtime`/`resolution` negatives, which only
//!   fail at runtime) → the source must parse successfully (ExpectOk),
//! - `negative.phase: early` → the source must fail static-semantic checks,
//!
//! The result distinguishes **false accepts** (we parse code that must be
//! rejected — pure correctness bugs) from **false rejects** (gaps + bugs in
//! constructs we don't yet support).

pub mod frontmatter;
pub mod runner;

pub use frontmatter::{FrontMatter, NegativePhase};
pub use runner::{
    classify_runtime, execute_test_source, run, run_runtime, Expect, Outcome, RuntimeExpect,
    RuntimeOutcome, RuntimeResult, RuntimeStats, Stats, TestResult, Variant,
};
