

pub struct BytePos(pub u32);

pub struct CharPos(pub usize);

/*
pub struct Source {
    pub source: String
}

impl Source {
    pub fn new(code:&str) -> Source{
        Source {
            source: String::from(code)
        }
    }
}
*/

pub struct Span {
   begin: CharPos,
   len: i64
}

impl Span {
    pub fn new(b:CharPos, l:i64) -> Span{
        Span {
            begin: b,
            len: l,
        }
    }
}