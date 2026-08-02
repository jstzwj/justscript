# Bytecode Architecture

Status: Bytecode V1 foundation; interpreter is the reference backend.

## Goals

The bytecode is the single executable contract shared by the interpreter, JIT
and AOT. A program must have the same JavaScript semantics in every backend;
native backends may optimize an opcode but may not reinterpret it.

The design borrows four boundaries from Hermes:

1. A compile-time bytecode module is immutable executable data plus metadata.
2. Functions are addressed through a module function table with an explicit
   entry function.
3. Instructions have declared operand kinds; string, constant, function and
   control-flow indices are never interchangeable by convention alone.
4. A runtime module resolves bytecode-local identities to realm-local cells and
   is the unit that future JIT code attaches to.

JustScript V1 remains stack-based while language coverage is completed. A
register conversion is deferred until measurements show that dispatch traffic
or JIT lowering justifies it. Changing evaluation architecture while object,
environment, completion and suspension semantics are still moving would hide
semantic defects inside allocation changes.

## Layering

```text
AST + static semantics
  -> bytecode compiler
  -> verified BytecodeModule (immutable contract)
  -> RuntimeModule (realm-bound strings/functions/modules/caches)
  -> baseline interpreter
  -> profiling + JIT for hot functions
  -> AOT serialization/native object emission
```

The interpreter remains normative: every new language feature lands there
with Test262 coverage before JIT or AOT lowering is enabled for its opcodes.
Unsupported native lowering falls back to the interpreter at a function or
basic-block boundary; it must not return a placeholder value.

## V1 Instruction Contract

`Instruction` currently uses one opcode and one `u16` immediate. Every opcode
declares an `OperandKind`: none, constant, local, upvalue, function, jump,
argument count or exception handler. `verify_module` checks those table and
control-flow references before execution. A verification failure is an engine
fault, never a JavaScript exception.

Semantic operations have semantic opcodes. In particular `>>>`, `in`, `void`
and the two forms of `delete` may not lower to `Shr`, `Add` or `Nop`. The same
rule applies to future module, environment, Promise and class instructions.

V1 keeps source spans parallel to instructions. `SourceId` / `SourceMap` now
assign stable identities to source snapshots, and runtime frames retain their
own source across module calls. The remaining migration changes each bytecode
entry to a compact cross-module `SourceSpan`; see `multi-module-source-map.md`.

## Required Runtime Structures

The next runtime layer introduces these records before adding feature opcodes:

- `RuntimeModule`: the first implementation now owns verified bytecode,
  import/export cells and link/evaluation state. Resolved string/property keys
  and inline-cache storage remain future extensions.
- `Environment`: declarative, function, module, object and global environment
  records with mutable/immutable binding cells and TDZ state.
- `Completion`: normal, return, throw, break and continue, so `finally`,
  generators and async suspension do not depend on ad hoc VM fields.
- `JobQueue`: Promise jobs and host jobs drained at script/module checkpoints.
- `Executable`: interpreter entry plus optional JIT entry, tiering counters and
  deoptimization/fallback metadata.

## Migration Sequence

1. Stabilize opcode metadata, verification and interpreter-only semantics.
2. Add the first `RuntimeModule`, module graph and live import/export cells on
   top of existing local cells; integrate source identities and cross-module
   VM function dispatch.
3. Replace the remaining flat local/global lookup with full environment records
   and binding opcodes. This is still required for TDZ, direct eval and complete
   strict/module semantics.
4. **In progress:** `Await`, FIFO Promise reactions, async function wrapping,
   top-level module state and the Test262 `$DONE` protocol are implemented for
   synchronously drainable jobs. Suspended async frames and full Promise
   resolution remain.
5. Complete object internal methods, property descriptors, classes and Proxy;
   lowering calls the same runtime abstract operations.
6. Fill standard built-ins, typed memory and host APIs against the runtime
   abstractions rather than VM-specific shortcuts.
7. Add per-function profiling and JIT entries. Compile only verified bytecode;
   fall back for unsupported opcodes.
8. Add a versioned serialized bytecode format and AOT artifacts only after the
   in-memory contract and module graph are stable.

## Explicit Non-Goals For V1

- No stable on-disk bytecode ABI yet.
- No unsafe direct-threaded dispatch until the verifier and fuzzing corpus are
  mature.
- No JIT/AOT semantic implementation separate from runtime abstract
  operations.
- No aggregate Test262 percentage across the five conformance profiles.
