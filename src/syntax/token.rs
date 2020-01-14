use self::super::keyword;
use self::super::punctuator;

pub struct Token {
    pub kind: TokenKind,
    pub len: usize,
}

impl Token {
    fn new(kind: TokenKind, len: usize) -> Token {
        Token { kind, len }
    }
}

pub enum TokenKind {
    WhiteSpace,
    LineTerminator,
    Comment,
    // common token
    IdentifierName,
    Punctuator,
    NumericLiteral,
    StringLiteral,
    Template,
    EOF,
}

