# JustScript: eliminate the final 41 language-runtime INCOMPLETE outcomes

## Objective

Eliminate all 41 remaining `INCOMPLETE` variants in the Test262
`language-runtime` profile by fixing shared engine semantics, not by adding
test-specific exceptions.

The starting baseline is commit `45dc450`:

| Outcome | Count |
|---|---:|
| PASS | 20,556 |
| FAIL | 11,329 |
| INCOMPLETE | 41 |
| SKIP | 12,549 |
| Executed | 31,926 |

The final acceptance condition is `INCOMPLETE = 0`. Every one of the original
41 variants should preferably become `PASS`; converting an internal/compiler/
module-host error to a normal runtime `FAIL` is useful diagnostic progress but
does not finish this task.

## Important worktree state

Do not reset, discard, or overwrite the current uncommitted changes. They are
intentional work in progress.

Current modified files:

- `crates/js-bytecode/src/compiler.rs`
- `crates/js-bytecode/src/local.rs`
- `crates/js-engine/src/module.rs`
- `crates/js-parser/src/parser.rs`
- `crates/js-parser/src/stmt.rs`
- `crates/js-syntax/src/ast/expr.rs`

The first three clusters already have working implementations in this tree:

1. `??` now uses real short-circuit control flow and preserves stack balance.
   This fixes the 16 computed-class-key variants whose visible error was
   `computed class key target is not a function`.
2. `compile_update` recursively unwraps `Expr::Paren`, fixing the 8 covered
   identifier update targets.
3. Function parameters reserve positional slots before allocating named local
   bindings. Duplicate sloppy parameters map their binding name to the last
   positional slot, fixing the 4 verifier failures.

The following representative tests were run after those changes and passed:

```text
expressions/class/cpn-class-expr-computed-property-name-from-expression-coalesce.js sloppy: PASS
expressions/postfix-increment/target-cover-id.js sloppy: PASS
function-code/S10.2.1_A2.js sloppy: PASS
function-code/S10.2.1_A3.js sloppy: PASS
```

At that point `cargo test -p js-bytecode -p js-engine --test milestone2` also
passed all 108 milestone tests.

The module request identity refactor in `crates/js-engine/src/module.rs` is only
partially applied. Therefore the current worktree intentionally does not
compile. `cargo check --workspace` currently reports six errors in
`crates/js-engine/src/pipeline.rs`:

- four dependency lookups still use `request.specifier` instead of the full
  `RequestEntry`;
- dependency insertion still inserts a `String`;
- `RuntimeModule` construction is missing `module_source_cell`;
- one diagnostic tries to format `RequestEntry` with `Display`.

Finish this refactor first. Do not work around it by reverting `RequestEntry`
to a string: import attributes are part of ModuleRequest identity, and text
self-import requires the same URL to have distinct JavaScript/text module
records.

## Required specification model

Use the current ECMA-262 specification algorithms as the design authority:

- Logical OR/AND/nullish evaluation and short-circuit completion values.
- UpdateExpression static semantics and transparent parentheses.
- FunctionDeclarationInstantiation for duplicate non-simple/simple parameter
  environments and positional arguments.
- ModuleRequest records, including phase and import attributes.
- ParseJSONModule and CreateDefaultExportSyntheticModule.
- CreateTextModule and CreateDefaultExportSyntheticModule.
- HostLoadImportedModule / FinishLoadingImportedModule cache identity.
- Source Text Module Record InitializeEnvironment and ResolveExport for the
  special `source` binding.
- Module Namespace Exotic Object resolution of re-exported source bindings.

Also follow Test262 `INTERPRETING.md`: each test gets a fresh realm; module
fixtures use host-visible file identities; async/module completion must use the
Test262 protocol.

## Parallel execution plan

Claude Code may launch multiple subagents, but agents must own disjoint files.
Do not let two agents edit `module.rs` or `pipeline.rs` concurrently.

### Agent A: bytecode/compiler invariants

Ownership:

- `crates/js-bytecode/src/compiler.rs`
- `crates/js-bytecode/src/local.rs`
- bytecode-focused tests

Tasks:

1. Review the existing nullish lowering. The required stack shape is exactly
   one completion value on both branches:

   ```text
   evaluate lhs
   dup
   JumpIfNullish rhs   // consumes duplicate
   Jump done           // original lhs is result
   rhs:
     pop               // discard original nullish lhs
     evaluate rhs
   done:
   ```

2. Add regression tests proving:
   - non-nullish LHS does not evaluate RHS;
   - null/undefined LHS evaluates RHS once;
   - a computed class key using `??` evaluates once and leaves the class
     constructor intact;
   - nested `??` is stack balanced.
3. Review recursive `Expr::Paren` handling for prefix and postfix update. Add
   tests for `(x)++`, `((x))--`, `++(x)`, and `--((x))`.
4. Review positional parameter slots. `param_count` must never exceed
   `locals.slot_count()`. Each formal parameter has a positional frame slot,
   while the lexical name map may point multiple names or a duplicate name to
   those slots. For duplicate sloppy parameters, the body binding must observe
   the last argument.
5. Test duplicate parameters in declarations, expressions, constructors, and
   closures. Confirm strict/non-simple duplicate parameters remain parser early
   errors rather than compiler faults.

Do not weaken the verifier. The verifier correctly detected the compiler's
broken frame layout.

### Agent B: source-phase parser and AST validation

Ownership:

- `crates/js-syntax/src/ast/expr.rs`
- `crates/js-parser/src/stmt.rs`
- `crates/js-parser/src/parser.rs`
- parser/static-semantics tests, if needed

Tasks:

1. Review the WIP `import source binding from "specifier"` parser.
2. Preserve contextual disambiguation:
   - `import source x from "m"` is phase `Source`;
   - `import source from "m"` is an eval-phase default import whose local name
     is `source`;
   - escaped `s\u006furce` must not match the `source` grammar terminal;
   - malformed source-phase forms must be syntax errors.
3. Ensure the AST retains `ImportPhase::Source` on the full `ModuleRequest`.
4. Add AST assertions, not merely `parse_module(...).is_ok()` assertions.
5. Run the parser suite and the five original parser/module fixtures.

The three source-phase entry tests originally surfaced as parser failures only
because their imported fixtures contain `import source ...`. Parsing alone is
not completion: Agent C must implement linking and runtime source bindings.

### Agent C: typed module host, JSON/text synthetic modules, source bindings

Ownership:

- `crates/js-engine/src/module.rs`
- `crates/js-engine/src/pipeline.rs`
- `crates/js-engine/Cargo.toml` if a dependency is needed
- `crates/js-engine/tests/modules.rs`
- `crates/js-test262/src/runner.rs`
- `crates/js-vm/src/builtins.rs` only for the Test262
  `%AbstractModuleSource%` host surface

This is the critical path and should be handled by one agent end to end.

#### C1. Finish ModuleRequest identity

The WIP `RequestEntry` contains:

```rust
specifier: String
phase: ImportPhase
attributes: Vec<(String, String)>
```

Attributes are sorted so source order does not affect identity. Complete the
pipeline conversion:

- `RuntimeModule.dependencies` remains keyed by full `RequestEntry`.
- Every lookup, insertion, export resolution, namespace collection, deferred
  evaluation dependency, and module evaluator traversal must use the full key.
- Error messages may print `request.specifier` plus attributes explicitly; do
  not add a misleading `Display` that drops phase/attributes.
- Deduplicate identical normalized requests, but keep different attributes as
  distinct requests.

Add a graph/module cache key similar to:

```rust
struct ModuleIdentity {
    canonical_url: String,
    module_type: ModuleType,
}

enum ModuleType {
    JavaScript,
    Json,
    Text,
}
```

The graph cache must distinguish the entry `text-self.js` loaded as JavaScript
from the same canonical URL requested with `{ type: "text" }`.

#### C2. Select and validate module type

Derive module type from normalized import attributes:

- no `type` attribute: JavaScript;
- `type: "json"`: JSON;
- `type: "text"`: text;
- unsupported types/attribute combinations: structured module load/link error,
  never an internal VM fault or panic.

Preserve attributes through static imports and re-exports. For literal dynamic
imports, preserve statically known `{ with: { type: ... } }` far enough for
preloading and cache lookup. The current compiler records only the specifier in
`BytecodeModule::dynamic_import_requests`; extend that metadata rather than
guessing from file extensions.

Dynamic import options must still be evaluated for side effects in source
order. A minimal correct lowering evaluates source, then options, discards the
options value after extracting host metadata at compile time when it is a
literal, and passes the source value to `DynamicImport`.

#### C3. JSON and text synthetic modules

Implement them as synthetic default-export module records with one immutable
`default` export and normal namespace behavior.

Acceptable near-term implementation: compile a generated internal module while
preserving the external canonical key:

- JSON: `export default JSON.parse(<quoted original source>);`
- text: `export default <quoted original source>;`

Use a structured string encoder such as `serde_json::to_string`; do not build
JavaScript string literals with manual replacements. Calling the realm's
`JSON.parse` preserves JSON semantics better than treating JSON as an object
literal (`"__proto__"`, duplicate keys, arrays, and JSON whitespace matter).

A cleaner implementation may construct a synthetic `CompiledModule` and
preinitialized export cell directly, but only do that if it fits the existing
`ModuleEvaluator` without special cases scattered across linking/evaluation.

Required behavior:

- exactly one `default` export;
- no named exports;
- namespace exposes the same live value;
- repeated imports with the same canonical URL and type share object identity;
- JSON arrays/objects use the current realm's Array/Object prototypes;
- text is never parsed as JavaScript;
- empty text yields `""`;
- self text import uses a distinct typed record and does not create a false
  JavaScript cycle.

#### C4. Source-phase binding and ModuleSource identity

`ImportedName::Source` is already introduced in the WIP.

Complete the model:

- Every runtime module owns one stable `module_source_cell`.
- A source-phase import binds its local immutable import binding directly to
  the target module's `module_source_cell`.
- Two source imports resolving to the same Module Record must share cell/value
  identity. This is required for non-ambiguous star re-export resolution.
- `export { localSourceBinding }` may remain a local export if its cell is the
  indirect source cell; `Cell::ptr_eq` must still identify the shared ultimate
  binding.
- Namespace access to the re-export returns the ModuleSource object.

The three tests check `instanceof $262.AbstractModuleSource`. Extend the
Test262 harness with an `AbstractModuleSource` constructor and create source
objects carrying that constructor identity. Keep this host-only surface out of
ordinary global installation.

The literal specifier `<module source>` is a Test262 host convention. Implement
a Test262-specific loader wrapper in `js-test262`, delegating normal paths to
`FileModuleLoader` and resolving this virtual specifier to one stable empty
module source. Do not hard-code the Test262 sentinel into the generic
filesystem loader.

### Integrator: merge, exact replay, full baseline

The integrator owns no feature implementation until Agents A-C finish. Then:

1. Review all diffs for duplicate abstractions and ensure the typed request is
   used consistently.
2. Run `cargo fmt --all`.
3. Run `cargo check --workspace`.
4. Run focused crate tests:

   ```bash
   cargo test -p js-parser
   cargo test -p js-bytecode
   cargo test -p js-engine --test milestone2
   cargo test -p js-engine --test modules
   cargo test -p js-test262
   ```

5. Build the runner once:

   ```bash
   cargo build -p js-test262
   ```

6. Extract the original 41 variants from the existing baseline before
   overwriting it:

   ```bash
   jq -r '.results[] | select(.outcome.status == "incomplete") |
     [.path, .variant, .outcome.reason] | @tsv' \
     target/test262-results/language-runtime/runtime.json
   ```

7. Replay every original variant with `execute-one`. Do not test only one file
   from each cluster. The command shape is:

   ```bash
   target/debug/js-test262 execute-one \
     /data2/wangjun/github/justscript/test262/test262 \
     test/language/<relative-test-path> <variant>
   ```

8. Run all workspace tests:

   ```bash
   cargo test --workspace
   ```

9. Run the full profile and write a new report:

   ```bash
   target/debug/js-test262 execute \
     /data2/wangjun/github/justscript/test262/test262 \
     --dir test/language \
     --json target/test262-results/language-runtime/runtime.json
   ```

10. Update `docs/test262-conformance.md` with exact counts and deltas. Explain
    any PASS/FAIL movement; never collapse profiles into one pass rate.
11. Run `git diff --check` and inspect `git status --short`.
12. Do not commit unless explicitly requested after review. If commits are
    desired, prefer two coherent commits:
    - compiler/parser/frame invariant fixes;
    - typed module host plus JSON/text/source-phase runtime.

## Original cluster acceptance matrix

| Cluster | Variants | Required outcome |
|---|---:|---|
| computed class key with coalesce | 16 | all PASS; no VM internal error |
| parenthesized update target | 8 | all PASS; no compile error |
| duplicate sloppy parameters | 4 | all PASS; verifier remains strict |
| JSON/text module linking | 8 | all PASS, including JSON idempotency |
| text type selection previously classified as parser | 2 | both PASS; source is never parsed as JS |
| source-phase parser/link/runtime | 3 | all PASS; no compile/module error |

Across the old host and parser buckets, JSON/text behavior covers six JSON
variants and four text behaviors: empty, JavaScript-looking source, string
content, and self import. The old report put 8 of these in module-host failures
and 2 JavaScript-looking text fixtures in parser failures because the loader
selected the wrong module type before parsing.

## Regression tests that must be added

At minimum add repository tests for:

- balanced `??` stack in a computed class key;
- covered update targets;
- duplicate positional parameters and last-argument visibility;
- same JSON module imported twice returns the same default object;
- JSON array/object prototype identity;
- a `.js` file imported as text is not parsed;
- JavaScript entry self-imported as text produces a distinct typed record;
- import-attribute source ordering does not alter request identity;
- same URL with different module types does alter record identity;
- source-phase import/re-export/namespace access shares one ModuleSource;
- Test262 virtual `<module source>` is handled only by its host loader.

## Constraints

- Do not add path- or filename-specific exceptions for the 41 tests.
- Do not infer module type from `.json` or `.js`; use import attributes.
- Do not weaken bytecode verification to hide compiler bugs.
- Do not classify a supported test as SKIP to reduce INCOMPLETE.
- Do not turn engine faults into generic JavaScript exceptions merely to alter
  Test262 taxonomy.
- Do not reset unrelated or pre-existing worktree changes.
- Keep Test262-only host conventions in `js-test262`, not the generic engine.
- Keep each test in a fresh Engine/realm as the runner currently does.

## Final handoff report

When implementation is finished, report:

1. commit(s), if any;
2. exact original-41 replay results;
3. workspace test result;
4. new full language-runtime counts and deltas from the baseline above;
5. remaining failures grouped by semantic root cause;
6. any deliberate limitation in dynamic import attributes or synthetic module
   representation.
