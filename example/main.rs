
use justscript::syntax::token::*;
use justscript::syntax::cursor::*;
use justscript::syntax::lexer::*;
use justscript::syntax::span::*;
use std::sync::Arc;

fn main() {
    println!("justscript starting...");
    /*
    let mut state = justscript::State::new();
    let hello_script = justscript::compile("var a = 1;");
    let result = state.run(&hello_script);
    */
    // run lexer
    let code = "
    // 这是一个变量
    var i = 10;
    /* 这是一个函数abc */
    function abc() {
        test = 1;
        return 1
    }
    ";
    let source_file = SourceFile::new(code);
    let mut reader = StringReader::new(Arc::new(source_file));

    for _i in 0..60 {
        println!("{:?}", reader.next_token());
    }
    
}
