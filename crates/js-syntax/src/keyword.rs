//! ECMAScript reserved words / keywords.
//!
//! This is the single canonical keyword enumeration used by the lexer, parser
//! and AST. Keeping it here eliminates the duplicated, partial keyword tables
//! the original prototype scattered across modules.

use std::str::FromStr;

/// A recognized ECMAScript keyword.
///
/// Includes both *reserved words* (e.g. `return`, `if`) and contextual keywords
/// (e.g. `of`, `as`) — the parser decides which set applies at a given point.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Keyword {
    // Declarations & scope
    Var,
    Let,
    Const,
    Function,
    Class,
    // Control flow
    If,
    Else,
    For,
    While,
    Do,
    Switch,
    Case,
    Default,
    Break,
    Continue,
    Return,
    Throw,
    Try,
    Catch,
    Finally,
    // Expressions & operators
    New,
    Delete,
    Typeof,
    Instanceof,
    In,
    Of,
    Void,
    This,
    Super,
    // Literals & types
    True,
    False,
    Null,
    Undefined,
    Async,
    Await,
    Yield,
    Enum,
    // Modifiers
    Static,
    Get,
    Set,
    // Module
    Import,
    Export,
    From,
    As,
    // Misc
    Debugger,
    Extends,
    With,
}

impl Keyword {
    /// All keywords and their source spellings.
    pub const ALL: &'static [(&'static str, Keyword)] = &[
        ("var", Keyword::Var),
        ("let", Keyword::Let),
        ("const", Keyword::Const),
        ("function", Keyword::Function),
        ("class", Keyword::Class),
        ("if", Keyword::If),
        ("else", Keyword::Else),
        ("for", Keyword::For),
        ("while", Keyword::While),
        ("do", Keyword::Do),
        ("switch", Keyword::Switch),
        ("case", Keyword::Case),
        ("default", Keyword::Default),
        ("break", Keyword::Break),
        ("continue", Keyword::Continue),
        ("return", Keyword::Return),
        ("throw", Keyword::Throw),
        ("try", Keyword::Try),
        ("catch", Keyword::Catch),
        ("finally", Keyword::Finally),
        ("new", Keyword::New),
        ("delete", Keyword::Delete),
        ("typeof", Keyword::Typeof),
        ("instanceof", Keyword::Instanceof),
        ("in", Keyword::In),
        ("of", Keyword::Of),
        ("void", Keyword::Void),
        ("this", Keyword::This),
        ("super", Keyword::Super),
        ("true", Keyword::True),
        ("false", Keyword::False),
        ("null", Keyword::Null),
        ("undefined", Keyword::Undefined),
        ("async", Keyword::Async),
        ("await", Keyword::Await),
        ("yield", Keyword::Yield),
        ("enum", Keyword::Enum),
        ("static", Keyword::Static),
        ("get", Keyword::Get),
        ("set", Keyword::Set),
        ("import", Keyword::Import),
        ("export", Keyword::Export),
        ("from", Keyword::From),
        ("as", Keyword::As),
        ("debugger", Keyword::Debugger),
        ("extends", Keyword::Extends),
        ("with", Keyword::With),
    ];

    /// The source spelling of this keyword.
    pub fn as_str(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, k)| *k == self)
            .map(|(s, _)| *s)
            .expect("every Keyword has a spelling")
    }
}

impl FromStr for Keyword {
    type Err = ();

    fn from_str(s: &str) -> Result<Keyword, ()> {
        Self::ALL
            .iter()
            .find(|(spelling, _)| *spelling == s)
            .map(|(_, k)| *k)
            .ok_or(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for (s, k) in Keyword::ALL {
            assert_eq!(k.as_str(), *s);
            assert_eq!(Keyword::from_str(s), Ok(*k));
        }
    }
}
