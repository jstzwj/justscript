//! ECMAScript punctuators (operators and delimiters).
//!
//! Like [`crate::keyword::Keyword`], this is the single canonical punctuator
//! table. The lexer parses maximal-munch punctuators and resolves them to one
//! of these variants.

/// A punctuator (a.k.a. token symbol / operator) in the ECMAScript grammar.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Punctuator {
    // Punctuation / delimiters
    LBrace,    // {
    RBrace,    // }
    LParen,    // (
    RParen,    // )
    LBracket,  // [
    RBracket,  // ]
    Dot,       // .
    Spread,    // ...
    Semicolon, // ;
    Comma,     // ,
    Colon,     // :
    Arrow,     // =>
    QuestionMark,    // ?
    NullishCoal,     // ??
    OptChain,        // ?.
    // Assignment
    Assign,     // =
    AddAssign,  // +=
    SubAssign,  // -=
    MulAssign,  // *=
    DivAssign,  // /=
    ModAssign,  // %=
    ExpAssign,  // **=
    BitAndAssign, // &=
    BitOrAssign,  // |=
    BitXorAssign, // ^=
    ShlAssign,    // <<=
    ShrAssign,    // >>=
    UshrAssign,   // >>>=
    AndAssign,    // &&=
    OrAssign,     // ||=
    NullishAssign, // ??=
    // Comparison
    Eq,        // ==
    NotEq,     // !=
    StrictEq,  // ===
    StrictNotEq, // !==
    Lt,        // <
    Gt,        // >
    Le,        // <=
    Ge,        // >=
    // Arithmetic
    Add,       // +
    Sub,       // -
    Mul,       // *
    Div,       // /
    Mod,       // %
    Exp,       // **
    Inc,       // ++
    Dec,       // --
    // Logical
    And,       // &&
    Or,        // ||
    Not,       // !
    // Bitwise
    BitAnd,    // &
    BitOr,     // |
    BitXor,    // ^
    BitNot,    // ~
    Shl,       // <<
    Shr,       // >>
    Ushr,      // >>>
    // Decorators (stage-3 proposal): leading `@` of `@decorator`.
    At,        // @
}

impl Punctuator {
    /// The canonical source spelling of this punctuator.
    pub fn as_str(self) -> &'static str {
        use Punctuator::*;
        match self {
            LBrace => "{",
            RBrace => "}",
            LParen => "(",
            RParen => ")",
            LBracket => "[",
            RBracket => "]",
            Dot => ".",
            Spread => "...",
            Semicolon => ";",
            Comma => ",",
            Colon => ":",
            Arrow => "=>",
            QuestionMark => "?",
            NullishCoal => "??",
            OptChain => "?.",
            Assign => "=",
            AddAssign => "+=",
            SubAssign => "-=",
            MulAssign => "*=",
            DivAssign => "/=",
            ModAssign => "%=",
            ExpAssign => "**=",
            BitAndAssign => "&=",
            BitOrAssign => "|=",
            BitXorAssign => "^=",
            ShlAssign => "<<=",
            ShrAssign => ">>=",
            UshrAssign => ">>>=",
            AndAssign => "&&=",
            OrAssign => "||=",
            NullishAssign => "??=",
            Eq => "==",
            NotEq => "!=",
            StrictEq => "===",
            StrictNotEq => "!==",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod => "%",
            Exp => "**",
            Inc => "++",
            Dec => "--",
            And => "&&",
            Or => "||",
            Not => "!",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            BitNot => "~",
            Shl => "<<",
            Shr => ">>",
            Ushr => ">>>",
            At => "@",
        }
    }
}
