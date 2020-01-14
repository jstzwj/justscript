
#[derive(Debug, Copy, Clone)]
pub struct BytePos(pub u32);

#[derive(Debug, Copy, Clone)]
pub struct CharPos(pub usize);


pub struct SourceFile {
    pub start_pos: BytePos,
    pub src: Option<String>
}

impl SourceFile {
    pub fn new(code:&str) -> SourceFile{
        SourceFile {
            start_pos: BytePos(0),
            src: Some(String::from(code))
        }
    }
}


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