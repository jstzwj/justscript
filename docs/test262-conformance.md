# Test262 Conformance Matrix

Test262 results are maintained as five independent profiles. There is no
project-wide aggregate pass rate because parsing, Early Errors, execution,
Annex B and ECMA-402 measure different contracts and have different skip rules.

| Profile | Test tree | Phases reported |
| --- | --- | --- |
| `language-front-end` | `test/language` | parse and Early Errors |
| `language-runtime` | `test/language` | runtime |
| `built-ins-runtime` | `test/built-ins` | runtime |
| `annexB` | `test/annexB` | front-end and runtime, separately |
| `intl402` | `test/intl402` | runtime |

Use the complete pinned checkout at `test262/test262`, not the sparse checkout
at `test262`. Reports are written below `target/test262-results/<profile>/` and
retain their own mode, Test262 revision, totals, skips and failure details.

```bash
# One profile
scripts/test262-matrix.sh language-front-end

# Every profile. Runtime profiles are intentionally long-running.
scripts/test262-matrix.sh all
```

The `annexB` directory contains `front-end.json` and `runtime.json`; they must
not be combined into one percentage. The other directories contain the phase
appropriate `front-end.json` or `runtime.json` report.

## Current Front-End Snapshot

Pinned Test262 revision:
`64ff467c0c1d60c077995bb7c5f93a9d8cc8ade1`.

| Profile | Files/variants | Pass | Fail | Incomplete | Skip |
| --- | ---: | ---: | ---: | ---: | ---: |
| `language-front-end` | 23,711 / 44,475 | 44,475 | 0 | 0 | 0 |
| `annexB` front-end | 1,086 / 1,377 | 1,162 | 215 | 0 | 0 |

## Current Language Runtime Snapshot

The complete pinned `test/language` runtime profile now has its first baseline.
The report is kept at `target/test262-results/language-runtime/runtime.json`.

| Profile | Files / variants | Executed | Pass | Fail | Incomplete | Skip |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `language-runtime` | 23,711 / 44,475 | 31,926 | 16,966 | 14,960 | 0 | 12,549 |

The pass rate over executed variants is 53.1%. This is not combined with the
front-end, built-ins, Annex B, or ECMA-402 profiles. `INCOMPLETE` is 0 and the
original 41 incomplete variants are all `PASS` (41 / 0 / 0).

The pass rate is lower than the previously reported 64.8% because that figure
was inflated by a Test262 async-protocol bug: `$DONE(value)` recorded only a
boolean and then threw inside a Promise reaction, where the throw was swallowed,
so any async test that signalled failure via `$DONE(error)` was misclassified
as `PASS`. Per `test262/INTERPRETING.md` an async failure signal must be judged
a failure, so the realm now retains the `$DONE` argument and the runner fails
when it is present. Correcting this reclassified **3797 variants** from a
false `PASS` to an honest `FAIL` — these are genuine async failures (async
generators, async functions, async destructuring, top-level-await rejection
propagation) that the engine does not yet fully implement and that were
previously hidden. None of the 3797 is caused by the prototype-chain or
dynamic-import work below; they are the necessary consequence of fixing the
async taxonomy. A runner regression test pins `$DONE(new Test262Error())` as a
mandatory `FAIL`.

The remaining work that finished the original 41:

Sixteen computed-class-key variants pass because nullish coalescing (`??`)
lowers to real short-circuit control flow that leaves exactly one completion
value on both branches; eight parenthesized update targets (`(x)++`, `++(x)`,
…) recursively unwrap `Expr::Paren` before dispatch; four
duplicate-sloppy-parameter variants reserve a positional frame slot per formal
parameter and map each binding name to the last positional slot, so the body
observes the final argument. The module host keys `RuntimeModule.dependencies`
and the graph cache by a typed `RequestEntry { specifier, phase, attributes }`
together with a `ModuleIdentity { canonical_url, module_type }`, so import
attributes are part of request identity: the same URL imported once as
JavaScript and once with `{ type: "text" }` yields two distinct Module Records.
JSON and text modules are synthetic default-export records. Per
`ParseJSONModule`, a JSON module's default export is produced by the engine's
intrinsic JSON parser at host load time (`js_vm::builtins::parse_json_intrinsic`)
— never by a realm-global `JSON.parse(...)` property access that a dependency
could have mutated before the module evaluates. The parsed value is injected
into the `*default*` local cell at load time and the synthetic module's bytecode
is never evaluated (it has no user code). The realm's own `%ArrayPrototype%` /
`%ObjectPrototype%` are linked onto arrays/objects in the result (preserving
`__proto__`, duplicate-key, array and whitespace semantics), and text is never
parsed as JavaScript. Dynamic `import(specifier, { with: { type } })` preserves
the phase and the literal attributes in the bytecode
(`DynamicModuleRequest`), and the `DynamicImport` opcode carries the **request
index** (not the specifier), so two imports of the same specifier with different
attributes resolve to distinct Module Records — the full ModuleRequest
(specifier + phase + attributes) is the identity, per TC39 Module Requests. The
options expression is still evaluated for side effects in source order.
Source-phase imports (`import source x from "m"`) bind a local immutable cell
directly to the target module's `module_source_cell`; re-exports and namespace
access resolve to one shared ModuleSource exposed as
`$262.AbstractModuleSource`, and the Test262-only `<module source>` virtual
specifier is resolved by a host loader wrapper in `js-test262`, never by the
generic filesystem loader.

The last of the original 41, `import/import-attributes/json-value-array.js`,
passes because the realm mints per-realm `%ObjectPrototype%` and
`%ArrayPrototype%` (in the reserved `realm.object_proto` / `realm.array_proto`
fields) and `Object.prototype` / `Array.prototype` resolve to those same
objects. `%ArrayPrototype%`'s own `[[Prototype]]` is `%ObjectPrototype%`
(`Object.getPrototypeOf(Array.prototype) === Object.prototype`, per
sec-properties-of-the-array-prototype-object). `Object()` and `new Object()`
produce ordinary objects linked to `%ObjectPrototype%`; the Object constructor
is `is_construct`-aware (the `NativeFn::call` trait carries the flag) so a
constructor call keeps the instance the construct path prepared rather than
minting a new one, which preserves subclassing (`class C extends Object {}`).

The VM intrinsics (`install_globals`, `globalThis`, the per-realm intrinsic
prototypes, and the `Array.prototype` wiring) run **once per realm** — guarded
by `realm.intrinsics_initialized` — not once per interpreter. A realm is
long-lived and shared across every interpreter an engine creates (`Engine::run`
/ `run_module` make a fresh interpreter per call), so re-bootstrapping per
execute would both mint fresh prototypes each call (breaking
`getPrototypeOf(objFromRun1) === Array.prototype` in run 2) and overwrite user
modifications to built-ins between runs.

`Array.prototype` is wired as a real property on the Array constructor at realm
bootstrap (mirroring `Object.prototype`), so the ordinary property walk finds
it. Ordinary array/object construction — array literals, `ObjectData` allocation,
`JSON.parse` results, `Array(...)`, and the `Object.keys` /
`Array.prototype.map` / `String.prototype.split` / `Promise.all` / `RegExp.exec`
result arrays — links `[[Prototype]]` to the realm prototypes via the current
interpreter's `array_prototype()` / `object_prototype()` accessors (read
straight from the realm). There is deliberately **no thread-local** prototype: a
process-wide thread-local would let a second interpreter on the same thread
overwrite the first's prototypes, breaking realm isolation, so the prototypes
are threaded explicitly through every creation path. Instance methods still
resolve through the builtin-method fallback (the prototypes themselves carry no
shadowing methods). As a side effect of the bytecode frame and control-flow
repairs, 54 previously-failing variants now pass; one flaky `for-in`
enumeration-order test moved pass→fail by non-deterministic key iteration,
which is pre-existing and
unrelated.

Logical assignment now lowers to short-circuit control flow while retaining a
single prepared Reference, and all truthiness consumers share one `ToBoolean`
implementation. Labelled statements carry explicit break/continue targets;
sloppy `with` uses object Environment Records with Proxy `has`,
`@@unscopables`, closure capture, and global object-record fallback.
Tagged-template sites have a per-Realm TemplateMap and frozen cooked/raw
arrays, while `import.meta` is cached per Module Record. Dynamic argument-list
bytecode drives IteratorRecord for ordinary, method, super, and constructor
calls. Async functions now suspend full frames into Promise reactions, and
async generators serialize concurrent requests through their request queue.

### Language Runtime Module Subset

The focused `test/language/module-code` runtime run is tracked as a diagnostic
subset of `language-runtime`, not as a sixth profile and not as the complete
language-runtime result.

| Files / variants | Executed | Pass | Fail | Incomplete | Skip |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 599 / 602 | 407 | 406 | 1 | 0 | 195 |

This snapshot includes real filesystem `_FIXTURE.js` loading, resolution
negatives, isolated realms, Promise jobs and async `$DONE`. The 99.8% rate over
executed cases must not be reported as the language-runtime or project-wide
Test262 pass rate. The three source-phase import/re-export tests pass: the
`import source` syntax is parsed with the correct phase, the source binding
links to the target module's `module_source_cell`, and the namespace re-export
resolves to the shared ModuleSource. The single failing case is
`top-level-await/dynamic-import-rejection.js`, an async test that was a false
`PASS` under the old `$DONE` bug (its rejection-propagation assertions do not
hold); it is now correctly classified as `FAIL` by the async-protocol fix.

Module environments now allocate direct mutable/immutable TDZ cells before
linking, replace import slots with initialized immutable indirect cells, and
instantiate hoisted functions only after those imports are linked. Unresolvable
references use GetValue semantics while `typeof` retains its specified special
case. Default exports use the synthetic `*default*` binding and NamedEvaluation
for anonymous function, generator, arrow, and class definitions.

Class execution now gives each evaluation a fresh private brand, propagates
private environments through element initializer and method closures, installs
base and derived instance elements at their construction boundaries, and
rejects duplicate private method/accessor installation. Static fields,
methods, accessors and blocks execute with the constructor as `this`; ordinary
property writes invoke accessor descriptors, and anonymous field definitions
receive their field names.

The focused `test/language/statements/class/elements` runtime subset currently
reports 1,534 files / 3,054 variants, with all 1,068 executed variants passing,
0 fail, 0 incomplete, and 1,986 skipped. Class methods and accessors carry an
explicit `[[HomeObject]]`; field arrows share the lexical `this` binding and
super environment. Computed keys run `ToPropertyKey`, including
`@@toPrimitive`, exactly once during definition. Public and private element
installation respects extensibility and descriptor invariants, while Proxy
ordinary operations remain distinct from private brand checks.

Direct eval retains its lexical/private/class execution context without leaking
a completed class's private environment. Generator suspension now preserves
that private-environment stack together with locals, handlers, and the shared
Iterator Record. Destructuring assignment and `for-in`/`for-of` assignment
targets use a common Reference preparation and PutValue path. BigInt literals
enter the constant pool as exact arbitrary-precision values rather than through
`f64`.

The standards-based front-end repairs have removed all false accepts from the
current `test/language` corpus. Block and Module Early Errors now compare the
specification's `LexicallyDeclaredNames`, `VarDeclaredNames`, `ExportedNames`,
and `ExportedBindings` collections. The module work removed 32 false accepts
and also fixed 6 valid combined-import forms that the prior AST could not
represent.
Import attributes and arbitrary module namespace names are now represented by
structured `ModuleRequest` and `ModuleExportName` nodes. This removed the 33
targeted failures plus 5 `import bytes` front-end failures; the complete
`test/language/module-code` directory is now 602 / 602.
Identifier tokenization now uses generated Unicode 17.0.0 `ID_Start` and
`ID_Continue` tables, including the `Other_ID_Start` and `Other_ID_Continue`
characters required by those properties. The generator pins both the Unicode
data URL and its SHA-256 digest. Sloppy-script `static` bindings are accepted,
while modules, classes and directive-induced strict mode retain their Early
Errors. The identifier and future-reserved-word directories are now 535 / 535
and 85 / 85 respectively.
Static deferred namespace imports now retain `defer` as the phase of their
structured `ModuleRequest`; ordinary imports and exports retain the evaluation
phase. Contextual-terminal lookahead preserves `import defer from "module"` as
an ordinary default import and rejects escaped or non-namespace deferred forms.
The complete `test/language/import/import-defer` front-end directory is now
107 / 107.

There are no failures in the pinned `language-front-end` profile. This result
only covers parsing and Early Errors: deferred module evaluation, namespace
trigger behaviour and the module loader remain runtime work and are not implied
by this front-end snapshot.

## Interpretation Rules

- A front-end pass means the parser and Early Error checker accepted or
  rejected the source in the required phase. It says nothing about execution.
- A runtime pass means the test actually executed in a fresh realm and met its
  expected completion. Skips and engine-incomplete outcomes are never passes.
- Unsupported Test262 harness includes remain skips until they are executed in
  order in the test realm.
- ECMA-402 is an explicit optional profile for this engine. Its result is never
  folded into ECMA-262 language conformance.
- Staging/proposal tests are not included in these five profiles. They will get
  a separate report after the relevant proposal/version policy is pinned.
