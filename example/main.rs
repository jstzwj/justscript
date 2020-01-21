
use justscript::parse::lexer::*;
use justscript::syntax::span::*;
use std::sync::Arc;

fn main() {
    println!("justscript starting...");

    let script = "var a = 'hello world!'";
    let mut state = justscript::State::new();
    let result = state.run(&script);
}
