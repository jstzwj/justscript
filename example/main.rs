
extern crate justscript;

fn main() {
    println!("justscript starting...");
    let mut state = justscript::State::new();
    let hello_script = justscript::compile("var a = 1;");
    let result = state.run(&hello_script);
    
}
