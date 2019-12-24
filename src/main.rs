
extern crate glow_core;

fn main() {
    let code = "123";
    println!("Hello, world!");
    glow_core::load::load_from_string(code);
}
