
use justscript::syntax::token::*;
use justscript::syntax::cursor::*;
use justscript::syntax::lexer::*;
use justscript::syntax::span::*;
use std::sync::Arc;

fn main() {
    println!("justscript starting...");
    // run lexer
    let code = "
    var i = 10;
    function abc() {
        test = 1;
        return 1
    }
    ";
    let source_file = SourceFile::new(code);
    let mut reader = StringReader::new(Arc::new(source_file));

    for _i in 0..10 {
        println!("{:?}", reader.next_token());
    }
    
}
