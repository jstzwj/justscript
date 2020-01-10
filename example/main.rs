
extern crate justscript;

fn main() {
    let code = "123";
    println!("Hello, world!");
    let mut state = justscript::State::new();
    state.addglobal("NaN", justscript::DataType::Null);
    
}
