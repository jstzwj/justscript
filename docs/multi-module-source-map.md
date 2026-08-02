# Multi-Module Source Map Memo

Status: phase 1 implemented; `SourceSpan` migration remains.

## Context

The diagnostic pipeline retains one `Arc<SourceFile>` per compiled
unit. AST spans are byte ranges local to that source, bytecode stores a
parallel PC-to-`Span` table, and VM failures carry the same source plus their
JavaScript stack. This is complete for scripts and independently compiled
modules, but a module graph can execute frames from several source files.

The single-source assumptions that must not leak into the module loader are:

- `DiagnosticReport` owns exactly one `SourceFile`.
- `BytecodeModule` owns one optional `SourceFile`.
- `JsException` and `EngineFault` retain a primary source, while each runtime
  frame now also retains the source of its defining bytecode module.
- A bare `Span` does not identify the file whose bytes it addresses.

## Decision

Keep the existing `Span` as a compact file-local byte range inside tokens and
AST nodes. Introduce source identity at cross-file boundaries:

```rust
pub struct SourceId(u32);

pub struct SourceSpan {
    pub source: SourceId,
    pub range: Span,
}

pub struct SourceMap {
    files: Vec<Arc<SourceFile>>,
    // canonical module URL/path -> SourceId
    identities: HashMap<String, SourceId>,
}
```

Every parsed `Program` belongs to one `SourceId`, so storing that ID on the
parse/compile unit is sufficient; duplicating it on every AST node is not. A
linker diagnostic that refers to multiple modules uses `SourceSpan` for its
primary label and notes.

`SourceId` values are stable for the lifetime of an `Engine`/realm. Canonical
module identity, not the importer's spelling of a specifier, determines whether
two imports share a `SourceId` and module instance.

## Target Flow

```text
ModuleLoader
  -> canonical specifier
  -> SourceMap::insert(SourceFile) -> SourceId
  -> ParseUnit { source_id, Program }
  -> CompiledModule { source_id, bytecode }
  -> bytecode PC -> SourceSpan
  -> RuntimeFrame { function, source_span }
  -> DiagnosticReport / JsException / EngineFault + Arc<SourceMap>
```

JavaScript exceptions remain language completions. `SourceMap` unifies their
location rendering with compiler diagnostics, but does not turn them into
`Diagnostic` values.

## Migration Order

1. **Implemented in part:** add `SourceId` and an engine-owned `SourceMap`
   without changing AST `Span`. `SourceSpan` is still pending.
2. Make `ParseSess` identify its source by `SourceId`; return a `ParseUnit`
   containing the `Program` and ID.
3. Change `Diagnostic`/`Note` locations and `DiagnosticReport` to resolve
   `SourceSpan` through `SourceMap`. Support a primary label in one module and
   notes in others.
4. Change bytecode source maps from `Vec<Span>` to `Vec<SourceSpan>` and remove
   `BytecodeModule::source`.
5. **Transitional implementation:** runtime frames carry their own
   `Arc<SourceFile>`, so cross-module stacks render correctly. Replace this with
   `SourceSpan` plus the graph's shared `SourceMap`.
6. **Implemented:** integrate loader canonicalization/cache and
   link/evaluate states. Cycles reuse existing module records and source
   identities.
7. Remove the temporary single-source fields from `CompiledModule` and public
   reporting APIs after all callers use `SourceMap`.

## Required Tests

- Two modules with identical byte offsets render diagnostics from the correct
  file.
- A stack `main.js -> a.js -> b.js` reports each frame using its own source.
- A linking error can place its primary label on an import and a note on the
  conflicting export in another module.
- Canonically equivalent specifiers reuse one `SourceId` and one evaluated
  module instance.
- Cyclic imports terminate and preserve correct cross-module stack frames.
- Runtime and backend faults do not fall back to dummy spans when a bytecode PC
  exists.

## Non-Goals

- Do not put paths, URLs, or `Arc<SourceFile>` directly into `Span`.
- Do not merge JavaScript exceptions with compiler diagnostics.
- Do not build the module loader around textual source concatenation.
- Do not make source-map lookup depend on the current working directory after
  specifier canonicalization.
