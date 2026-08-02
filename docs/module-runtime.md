# Module Runtime

Status: ordinary modules plus the first asynchronous/deferred execution
milestone.

## Boundaries

The implementation follows the same compile-time/runtime separation used by
the bytecode architecture:

```text
ModuleLoader resolve/load
  -> SourceMap + parse Module goal
  -> CompiledModule { Program, BytecodeModule }
  -> ModuleGraph / RuntimeModule
  -> link + instantiate shared binding cells
  -> dependency-ordered bytecode evaluation
```

`ModuleLoader` is the host boundary. `MemoryModuleLoader` provides deterministic
embedding/tests; `FileModuleLoader` canonicalizes filesystem paths before the
graph cache sees them. The engine never concatenates sources and never asks the
VM to resolve paths.

Each runtime module moves through `Unlinked`, `Linking`, `Linked`, `Evaluating`,
`EvaluatingAsync`, `Evaluated`, or `Errored`. Direct binding cells retain TDZ
and mutability state; import cells are immutable indirect references. This
keeps exporter updates live without allowing assignment through an import.

VM functions carry both a function-table index and a bytecode-module index.
This is required when an imported function is called by another module: its
constants, nested functions, source map and closure descriptors all belong to
the defining bytecode module.

## Implemented

- graph resolve/load/cache with canonical filesystem identities;
- module-goal parse, Early Errors and bytecode compilation;
- named/default imports and local exports;
- named re-exports and star resolution for named imports;
- shared live binding cells, lexical TDZ and immutable import/const writes;
- cyclic graph linking and synchronous dependency evaluation;
- module-instantiation-time function initialization;
- sorted Module Namespace Exotic Objects with live export cells, null
  prototype behavior at the object boundary, and rejected writes/deletes;
- cross-module function calls, closures and per-frame source rendering;
- structured resolve, load, link and unsupported-feature errors.
- Test262 module entries, relative `_FIXTURE.js` loading, resolution-negative
  classification, isolated realms and async `$DONE` observation;
- FIFO Promise reaction jobs, `Promise.resolve/reject`, constructor,
  `then`/`catch`, async function promises and the `Await` bytecode opcode;
- top-level await for synchronously drainable Promise graphs;
- `import defer` namespace triggers that skip eager dependency evaluation and
  evaluate the deferred graph on the first observable namespace operation.

## Next

- true suspended async module execution for Promises that remain pending after
  the current job queue, including spec DFS async-parent ordering;
- thenable assimilation, Promise combinators and unhandled rejection tracking;
- dynamic `import()`, source-phase imports, JSON modules and attribute-aware
  host module identity;
- the remaining namespace internal methods (`[[DefineOwnProperty]]`, symbol
  keys and complete descriptors) once the common object descriptor layer is
  implemented;
- full `$262` host API, additional harness includes, agents and detach support.

The current event loop is deliberately single-threaded and synchronously
driven. A top-level await whose Promise is still pending after the available
FIFO jobs is reported as incomplete rather than guessed. Deferred namespace
evaluation uses the same interpreter and instantiated cells, but async
deferred dependencies still need the full suspended-module DFS algorithm.
