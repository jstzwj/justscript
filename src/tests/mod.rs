#[cfg(test)]

use self::super::*;
use std::sync::Arc;

#[test]
fn test_lexer() {
    println!("justscript lexer starting...");
    // run lexer
    let code = "
    var i = 10;
    function abc() {
        test = 1;
        return 1
    }
    ";
    let source_file = syntax::span::SourceFile::new(code);
    let mut reader = syntax::lexer::StringReader::new(Arc::new(source_file));

    for _i in 0..10 {
        println!("{:?}", reader.next_token());
    }
}