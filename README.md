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
```

[Cranelift]: https://github.com/bytecodealliance/wasmtime/tree/main/cranelift
