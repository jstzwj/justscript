use self::super::keyword;
use self::super::punctuator;

pub struct Token {
    pub kind: TokenKind,
    pub len: usize,
}

impl Token {
    pub fn new(kind: TokenKind, len: usize) -> Token {
        Token { kind, len }
    }
}

pub enum TokenKind {
    WhiteSpace,
    LineTerminator,
    MultiLineComment,
    SingleLineComment,
    // common token
    IdentifierName,
    Punctuator,
    NumericLiteral,
    StringLiteral,
    Template,
    EOF,
}

