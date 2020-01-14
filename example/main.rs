
extern crate justscript;

fn main() {
    let code = "123";
    println!("Hello, world!");
    let mut state = justscript::State::new();
    let hello_script = justscript::compile("var a = 1;");
    let result = state.run(&hello_script);
    
}
