pub struct Token {
    pub data: TokenData,
    pub pos: Position,
}

pub enum TokenData {
    NullLiteral,
    BooleanLiteral(bool),
    NumericLiteral(f64),
    StringLiteral(String),
    EOF,
    Identifier(String),
    Keyword(Keyword),
    Punctuator(Punctuator),
    Comment(String),
}
