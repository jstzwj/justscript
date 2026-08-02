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

Runtime baselines must be regenerated from the complete checkout. Historical
runtime numbers from the sparse checkout are deliberately not recorded here.
Until a complete run exists, a profile is **unmeasured**, never implicitly
passing.

### Language Runtime Module Subset

The focused `test/language/module-code` runtime run is tracked as a diagnostic
subset of `language-runtime`, not as a sixth profile and not as the complete
language-runtime result.

| Files / variants | Executed | Pass | Fail | Incomplete | Skip |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 599 / 602 | 407 | 290 | 61 | 56 | 195 |

This snapshot includes real filesystem `_FIXTURE.js` loading, resolution
negatives, isolated realms, Promise jobs and async `$DONE`. The 71.3% rate over
executed cases must not be reported as the language-runtime or project-wide
Test262 pass rate. Current actionable clusters are top-level await (55), module
namespace internal methods (26), and source-phase imports (2); many of those
depend on class inheritance, object methods/spread, Symbol/property descriptor
support and dynamic import rather than the loader itself.

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
