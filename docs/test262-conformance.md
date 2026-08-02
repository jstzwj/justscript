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
| `language-runtime` | 23,711 / 44,475 | 31,926 | 19,990 | 10,766 | 1,170 | 12,549 |

The pass rate over executed variants is 62.6%. This is not combined with the
front-end, built-ins, Annex B, or ECMA-402 profiles. The largest incomplete
clusters are unsupported statement lowering (454), pending async continuations
(246), spread calls (170), unsupported expression lowering (102), spread in
`new` (44), and the three legacy logical opcodes (113 total).

### Language Runtime Module Subset

The focused `test/language/module-code` runtime run is tracked as a diagnostic
subset of `language-runtime`, not as a sixth profile and not as the complete
language-runtime result.

| Files / variants | Executed | Pass | Fail | Incomplete | Skip |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 599 / 602 | 407 | 404 | 0 | 3 | 195 |

This snapshot includes real filesystem `_FIXTURE.js` loading, resolution
negatives, isolated realms, Promise jobs and async `$DONE`. The 99.3% rate over
executed cases must not be reported as the language-runtime or project-wide
Test262 pass rate. The remaining outcomes are three source-phase
import/re-export tests whose syntax is not part of the pinned ES module
front-end yet.

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
