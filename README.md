# JustScript

A lightweight **JavaScript engine written in Rust**, designed from the ground up
to support three execution modes:

- **Interpreter** — a stack-based bytecode virtual machine.
- **JIT** — just-in-time native compilation via [Cranelift].
- **AOT** — ahead-of-time compilation to native object files via [Cranelift].

> Status: **early scaffold**. The module layout and public APIs are in place;
> individual passes are being filled in incrementally.

## Architecture

JustScript is organized as a Cargo **workspace** of focused crates, with strict
acyclic dependencies (`js-syntax` is the leaf, `js-cli` is the root):

```
                  js-syntax  ◀──── shared by almost every crate
                     ▲
        ┌────────────┼─────────────┐
   js-lexer   js-diagnostics   js-runtime
        ▲          ▲               ▲
        └────┐     │      ┌────────┘
         js-parser │      │
              ▲    │      │
            js-bytecode
                 ▲
        ┌────────┴────────┐
      js-vm          js-codegen  (Cranelift, features: jit / aot)
                 ▲
             js-engine
                 ▲
              js-cli
```

| Crate | Responsibility |
| --- | --- |
| [`js-syntax`](crates/js-syntax) | Source positions, tokens, keywords, punctuators, AST. |
| [`js-lexer`](crates/js-lexer) | Character stream cursor + tokenizer. |
| [`js-diagnostics`](crates/js-diagnostics) | Span-aware diagnostics + `DiagResult`. |
| [`js-parser`](crates/js-parser) | Recursive-descent + Pratt parser → AST. |
| [`js-runtime`](crates/js-runtime) | `Value`, `Object`/`Shape`, GC, builtins, realm. |
| [`js-bytecode`](crates/js-bytecode) | Opcode set, bytecode functions, AST → bytecode. |
| [`js-vm`](crates/js-vm) | Bytecode interpreter / dispatch loop. |
| [`js-codegen`](crates/js-codegen) | Cranelift JIT + AOT backends. |
| [`js-engine`](crates/js-engine) | Top-level `Engine` API, execution-mode pipeline. |
| [`js-cli`](crates/js-cli) | REPL + file runner (`--interpret` / `--jit` / `--aot`). |

### Diagnostics and execution failures

Source identity and locations are retained through the complete pipeline:

```text
SourceFile -> tokens/AST Span -> DiagnosticReport
                           \-> bytecode PC-to-Span map
                                      \-> JsException | EngineFault + stack
```

The public engine API uses one failure taxonomy:

- `EngineError::Compile` contains source-bound parser, early-error, or bytecode
  diagnostics.
- `EngineError::Module` represents host resolution/loading and module linking
  failures; it is distinct from JavaScript exceptions and VM faults.
- `EngineError::Exception` is an uncaught JavaScript value with its throw site
  and JavaScript call stack. It is a language completion, not a compiler error.
- `EngineError::Fault` represents a VM/backend bug or unsupported execution
  path, also with its source location and stack.

`Engine::run` returns this taxonomy as a `Result`; `Engine::execute` exposes the
same error through `ExecutionOutcome` for hosts that prefer exhaustive matching.
The multi-module source migration is tracked in
[`docs/multi-module-source-map.md`](docs/multi-module-source-map.md).

## Usage

```bash
# Build the whole workspace
cargo build --workspace

# Run a file
cargo run -p js-cli -- run script.js --interpret

# Enable the JIT/AOT backends
cargo build --workspace --features js-codegen/jit --features js-codegen/aot

# Run the test suite
cargo test --workspace

# Run one independently reported Test262 profile
scripts/test262-matrix.sh language-front-end
```

Test262 conformance is reported as five non-aggregated profiles; see
[`docs/test262-conformance.md`](docs/test262-conformance.md). The bytecode and
execution-backend contract is documented in
[`docs/bytecode-architecture.md`](docs/bytecode-architecture.md); the module
loader/linker/runtime contract is documented in
[`docs/module-runtime.md`](docs/module-runtime.md).

[Cranelift]: https://github.com/bytecodealliance/wasmtime/tree/main/cranelift
