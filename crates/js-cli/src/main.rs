//! The JustScript command-line: `run <file>` / `eval <code>` / `repl`, with an
//! `--mode` flag selecting the execution backend.

use clap::{Parser, Subcommand, ValueEnum};
use js_engine::{Engine, EngineConfig, EngineError, ExecutionMode};
use js_syntax::ProgramKind;

#[derive(Parser)]
#[command(
    name = "justscript",
    version,
    about = "A Rust JavaScript engine (interpret / JIT / AOT)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,

    /// Execution backend to use.
    #[arg(long, value_enum, global = true, default_value_t = ModeArg::Interpret)]
    mode: ModeArg,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a script file.
    Run { path: String },
    /// Evaluate a source string.
    Eval { source: String },
    /// Start a read-eval-print loop.
    Repl,
    /// Print version and backend info.
    Info,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ModeArg {
    Interpret,
    Jit,
    Aot,
}

impl ModeArg {
    fn to_mode(self) -> ExecutionMode {
        match self {
            ModeArg::Interpret => ExecutionMode::Interpret,
            ModeArg::Jit => ExecutionMode::Jit,
            ModeArg::Aot => ExecutionMode::Aot,
        }
    }
}

fn main() {
    let cli = Cli::parse();
    let config = EngineConfig {
        mode: cli.mode.to_mode(),
        ..EngineConfig::default()
    };

    let result = match cli.cmd {
        Cmd::Run { path } => {
            let src = match std::fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("error: cannot read {path}: {e}");
                    std::process::exit(1);
                }
            };
            run_once(&config, &path, &src)
        }
        Cmd::Eval { source } => run_once(&config, "<eval>", &source),
        Cmd::Repl => {
            eprintln!("REPL not implemented yet (skeleton)");
            Ok(())
        }
        Cmd::Info => {
            print_info();
            Ok(())
        }
    };

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run_once(config: &EngineConfig, name: &str, src: &str) -> Result<(), EngineError> {
    let mut engine = Engine::new(config.clone());
    let result = engine.run_named(name, src, ProgramKind::Script)?;
    println!("{:?}", result.value);
    Ok(())
}

fn print_info() {
    println!("justscript — Rust JavaScript engine (skeleton)");
    println!(
        "backends: interpret (default){jit}{aot}",
        jit = if cfg!(feature = "jit") { " +jit" } else { "" },
        aot = if cfg!(feature = "aot") { " +aot" } else { "" },
    );
}
