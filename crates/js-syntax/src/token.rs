//! Lexical tokens.
//!
//! [`TokenKind`] is the single canonical enumeration of token categories,
//! produced by `js-lexer` and consumed by `js-parser`. A [`Token`] pairs a kind
//! with the byte [`crate::Span`] it covers in the source — text is never copied
//! into the token, it is sliced on demand via `Span::snippet`.

use crate::keyword::Keyword;
use crate::punctuator::Punctuator;
use crate::source::Span;

/// The category + payload of a single token.
#[derive(Clone, Debug)]
pub enum TokenKind {
    /// Trivia: whitespace (spaces, tabs) — usually filtered before parsing.
    Whitespace,
    /// Trivia: a line terminator (`\n`, `\r`, `\r\n`).
    LineTerminator,
    /// Trivia: `// ...` or `/* ... */`.
    Comment { is_block: bool },
    /// An identifier; `raw` is the *decoded* identifier text (Unicode escapes
    /// resolved). Keyword classification is done separately via [`Keyword::from_str`].
    Ident(String),
    /// A keyword recognized during lexing.
    Keyword(Keyword),
    /// `undefined` — lexically an identifier, but tagged for convenience.
    /// (Kept as Ident in practice; this variant is reserved for future use.)
    /// A punctuator.
    Punctuator(Punctuator),
    /// A numeric literal as *raw source text*; the parser decides the base and
    /// converts to a value. Storing raw text preserves `0b101`, `1e3`, etc.
    Numeric(String),
    /// A string literal, *already unescaped* (escapes resolved, quotes stripped).
    String(String),
    /// A bigint literal as raw text (e.g. `123n`).
    Bigint(String),
    /// A regex literal: the full `patternflags` source between slashes.
    Regex { pattern: String, flags: String },
    /// A template string chunk. Template literals are tokenized in multiple
    /// parts by the lexer; this carries the raw text of one segment.
    Template { raw: String, cooked: Option<String> },
    /// Private name `#foo`.
    PrivateName(String),
    /// End of input.
    Eof,
    /// A token the lexer could not classify. The parser turns this into a
    /// diagnostic rather than panicking.
    Unknown(char),
}

impl PartialEq for TokenKind {
    fn eq(&self, other: &TokenKind) -> bool {
        use TokenKind::*;
        match (self, other) {
            (Whitespace, Whitespace) | (LineTerminator, LineTerminator) | (Eof, Eof) => true,
            (Comment { is_block: a }, Comment { is_block: b }) => a == b,
            (Ident(a), Ident(b)) => a == b,
            (Keyword(a), Keyword(b)) => a == b,
            (Punctuator(a), Punctuator(b)) => a == b,
            (Numeric(a), Numeric(b)) => a == b,
            (String(a), String(b)) => a == b,
            (Bigint(a), Bigint(b)) => a == b,
            (
                Regex { pattern: p1, flags: f1 },
                Regex { pattern: p2, flags: f2 },
            ) => p1 == p2 && f1 == f2,
            (Template { raw: r1, .. }, Template { raw: r2, .. }) => r1 == r2,
            (PrivateName(a), PrivateName(b)) => a == b,
            (Unknown(a), Unknown(b)) => a == b,
            _ => false,
        }
    }
}

impl TokenKind {
    /// Whether this token is trivia (whitespace, line terminators, comments)
    /// and should be skipped before parsing.
    pub fn is_trivia(&self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace | TokenKind::LineTerminator | TokenKind::Comment { .. }
        )
    }
}

/// A token: its [`TokenKind`] plus the byte [`Span`] it occupies in the source.
#[derive(Clone, Debug)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Token {
        Token { kind, span }
    }

    /// Convenience: extract this token's text from the owning source string.
    pub fn snippet<'a>(&self, src: &'a str) -> Option<&'a str> {
        self.span.snippet(src)
    }
}
