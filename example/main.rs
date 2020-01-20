
use justscript::syntax::token::*;
use justscript::parse::lexer::*;
use justscript::syntax::span::*;
use std::sync::Arc;

fn main() {
    println!("justscript starting...");
    /*
    let script = "var a = 'hello world!'";
    let mut state = justscript::State::new();
    let result = state.run(&script);
    */
    // run lexer
    let code = "
    // 这是一个变量
    var i = 10;
    j = '1234, hello world! \n'
    /* 这是一个函数abc */
    function abc() {
        test = 1;
        return 1.0
    }
    ";
    let source_file = SourceFile::new(code);
    let mut reader = StringReader::new(Arc::new(source_file));

    for _i in 0..80 {
        println!("{:?}", reader.next_token());
    }
    
}
