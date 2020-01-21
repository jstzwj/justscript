use self::super::keyword;
use self::super::punctuator;
use self::super::span::Span;



#[derive(Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum TokenKind {
    Unknown,
    WhiteSpace,
    LineTerminator,
    /// comment
    MultiLineComment,
    SingleLineComment,
    /// common token
    /// Indentifier 
    /// At this step keywords are also considered identifiers.
    Ident,
    /// Punctuator
    /// ";"
    Semi,
    /// ","
    Comma,
    /// "."
    Dot,
    /// "("
    OpenParen,
    /// ")"
    CloseParen,
    /// "{"
    OpenBrace,
    /// "}"
    CloseBrace,
    /// "["
    OpenBracket,
    /// "]"
    CloseBracket,
    /// "@"
    At,
    /// "#"
    Pound,
    /// "~"
    Tilde,
    /// "?"
    Question,
    /// ":"
    Colon,
    /// "$"
    Dollar,
    /// "="
    Eq,
    /// "!"
    Not,
    /// "<"
    Lt,
    /// ">"
    Gt,
    /// "-"
    Minus,
    /// "&"
    And,
    /// "|"
    Or,
    /// "+"
    Plus,
    /// "*"
    Star,
    /// "/"
    Slash,
    /// "^"
    Caret,
    /// "%"
    Percent,
    /// "/="
    DivEq,
    /// "="
    Equal,
    /// "=="
    Equal2,
    /// "==="
    Equal3,
    /// literal
    // NumericLiteral
    DecimalLiteral,
    BinaryIntegerLiteral,
    OctalIntegerLiteral,
    HexIntegerLiteral,
    // StringLiteral
    StringLiteral,
    // Template
    Template,
    EOF,
}


pub enum PunctuatorType {
    
}
