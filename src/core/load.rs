use std::path::Path;
use std::fs;

pub fn load_from_string(code:&str) {

}


pub fn load_from_file(path:&str) {
    let contents = fs::read_to_string(path)
        .expect("Something went wrong reading the file");
    load_from_string(&contents);
}