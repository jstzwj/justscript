use crate::syntax::token::TokenKind;


#[derive(Debug)]
pub struct LexToken {
    pub kind: TokenKind,
    pub len: usize,
}

impl LexToken {
    pub fn new(kind: TokenKind, len: usize) -> LexToken {
        LexToken { kind, len }
    }
}